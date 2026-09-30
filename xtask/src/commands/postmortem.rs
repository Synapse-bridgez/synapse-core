use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::Args;
use serde::Deserialize;
use std::{fs, path::PathBuf, process::Command as ProcessCommand};

const DEFAULT_METRIC_QUERIES: &[(&str, &str)] = &[
	("Request rate", "sum(rate(http_request_duration_ms_count[5m]))"),
	("Error rate", "sum(rate(http_request_duration_ms_count{status=~\"5..\"}[5m]))"),
	("Database pool saturation", "max(db_pool_active_connections / (db_pool_active_connections + db_pool_idle_connections))"),
];

#[derive(Args)]
pub struct PostmortemArgs {
	/// Incident start time in RFC3339 format.
	#[arg(long)]
	start: String,

	/// Incident end time in RFC3339 format.
	#[arg(long)]
	end: String,

	/// Alertmanager base URL (also read from ALERTMANAGER_URL).
	#[arg(long, env = "ALERTMANAGER_URL")]
	alertmanager_url: Option<String>,

	/// Optional alert-backend bearer token (also read from ALERTMANAGER_BEARER_TOKEN).
	#[arg(long, env = "ALERTMANAGER_BEARER_TOKEN", hide_env_values = true)]
	alertmanager_token: Option<String>,

	/// Prometheus base URL (also read from PROMETHEUS_URL).
	#[arg(long, env = "PROMETHEUS_URL")]
	prometheus_url: Option<String>,

	/// Optional Prometheus bearer token (also read from PROMETHEUS_BEARER_TOKEN).
	#[arg(long, env = "PROMETHEUS_BEARER_TOKEN", hide_env_values = true)]
	prometheus_token: Option<String>,

	/// GitHub repository in owner/name form for release and deployment history.
	#[arg(long, env = "GITHUB_REPOSITORY")]
	github_repo: Option<String>,

	/// Optional GitHub token (also read from GITHUB_TOKEN).
	#[arg(long, env = "GITHUB_TOKEN", hide_env_values = true)]
	github_token: Option<String>,

	/// Prometheus query as NAME=EXPRESSION; may be repeated to replace the defaults.
	#[arg(long = "metric-query")]
	metric_queries: Vec<String>,

	/// Sampling interval for metric snapshots in seconds.
	#[arg(long, default_value_t = 60)]
	step_secs: i64,

	/// Output markdown path.
	#[arg(long, default_value = "postmortem-draft.md")]
	output: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
struct Alert {
	#[serde(default)]
	labels: std::collections::BTreeMap<String, String>,
	#[serde(default)]
	annotations: std::collections::BTreeMap<String, String>,
	#[serde(rename = "startsAt")]
	starts_at: String,
	#[serde(rename = "endsAt")]
	ends_at: String,
	#[serde(default)]
	status: AlertStatus,
	#[serde(rename = "generatorURL", default)]
	generator_url: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct AlertStatus {
	state: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Release {
	tag_name: String,
	#[serde(default)]
	name: String,
	#[serde(default)]
	html_url: String,
	published_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Deployment {
	id: u64,
	sha: String,
	#[serde(rename = "ref")]
	ref_name: String,
	environment: String,
	task: String,
	created_at: String,
	#[serde(default)]
	description: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PrometheusResponse {
	status: String,
	#[serde(default)]
	error: Option<String>,
	#[serde(default)]
	data: Option<PrometheusData>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PrometheusData {
	#[serde(default)]
	result: Vec<PrometheusSeries>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PrometheusSeries {
	#[serde(default)]
	metric: std::collections::BTreeMap<String, String>,
	#[serde(default)]
	values: Vec<(f64, String)>,
}

#[derive(Debug)]
struct MetricSnapshot {
	name: String,
	expression: String,
	series: Vec<PrometheusSeries>,
}

#[derive(Debug, Default)]
struct IncidentHistory {
	alerts: Vec<Alert>,
	metrics: Vec<MetricSnapshot>,
	releases: Vec<Release>,
	deployments: Vec<Deployment>,
	prometheus_url: Option<String>,
	source_notes: Vec<String>,
}

fn parse_timestamp(value: &str, name: &str) -> Result<DateTime<Utc>> {
	DateTime::parse_from_rfc3339(value)
		.with_context(|| format!("{name} must be an RFC3339 timestamp"))
		.map(|timestamp| timestamp.with_timezone(&Utc))
}

fn curl_json<T: for<'de> Deserialize<'de>>(
	url: &str,
	token: Option<&str>,
	github: bool,
) -> Result<T> {
	let mut command = ProcessCommand::new("curl");
	command.args(["-fsS", "--max-time", "60"]);
	if let Some(token) = token.filter(|token| !token.is_empty()) {
		command.arg("-H").arg(format!("Authorization: Bearer {token}"));
	}
	if github {
		command
			.arg("-H")
			.arg("Accept: application/vnd.github+json")
			.arg("-H")
			.arg("X-GitHub-Api-Version: 2022-11-28");
	}
	let output = command
		.arg(url)
		.output()
		.context("running curl to fetch incident history")?;
	if !output.status.success() {
		bail!("request failed: {}", String::from_utf8_lossy(&output.stderr).trim());
	}
	serde_json::from_slice(&output.stdout).context("parsing incident history response")
}

fn parse_metric_queries(queries: &[String]) -> Result<Vec<(String, String)>> {
	if queries.is_empty() {
		return Ok(DEFAULT_METRIC_QUERIES
			.iter()
			.map(|(name, expression)| ((*name).to_owned(), (*expression).to_owned()))
			.collect());
	}
	queries
		.iter()
		.map(|query| {
			let (name, expression) = query
				.split_once('=')
				.with_context(|| format!("metric query {query:?} must use NAME=EXPRESSION"))?;
			if name.trim().is_empty() || expression.trim().is_empty() {
				bail!("metric query name and expression must not be empty");
			}
			Ok((name.trim().to_owned(), expression.trim().to_owned()))
		})
		.collect()
}

fn query_prometheus(
	base_url: &str,
	token: Option<&str>,
	name: String,
	expression: String,
	start: i64,
	end: i64,
	step_secs: i64,
) -> Result<MetricSnapshot> {
	let mut command = ProcessCommand::new("curl");
	command
		.args(["-fsS", "--get", "--max-time", "60"])
		.arg("--data-urlencode")
		.arg(format!("query={expression}"))
		.arg("--data-urlencode")
		.arg(format!("start={start}"))
		.arg("--data-urlencode")
		.arg(format!("end={end}"))
		.arg("--data-urlencode")
		.arg(format!("step={step_secs}"));
	if let Some(token) = token.filter(|token| !token.is_empty()) {
		command.arg("-H").arg(format!("Authorization: Bearer {token}"));
	}
	let url = format!("{}/api/v1/query_range", base_url.trim_end_matches('/'));
	let output = command
		.arg(url)
		.output()
		.context("running curl for Prometheus query_range")?;
	if !output.status.success() {
		bail!("Prometheus request failed: {}", String::from_utf8_lossy(&output.stderr).trim());
	}
	let response: PrometheusResponse =
		serde_json::from_slice(&output.stdout).context("parsing Prometheus range response")?;
	if response.status != "success" {
		bail!("Prometheus query failed: {}", response.error.unwrap_or_default());
	}
	Ok(MetricSnapshot {
		name,
		expression,
		series: response.data.unwrap_or_default().result,
	})
}

fn time_in_window(value: &str, start: DateTime<Utc>, end: DateTime<Utc>) -> bool {
	DateTime::parse_from_rfc3339(value)
		.map(|time| {
			let time = time.with_timezone(&Utc);
			time >= start && time <= end
		})
		.unwrap_or(false)
}

fn alert_overlaps_window(alert: &Alert, start: DateTime<Utc>, end: DateTime<Utc>) -> bool {
	let Ok(alert_start) = DateTime::parse_from_rfc3339(&alert.starts_at) else {
		return false;
	};
	let Ok(alert_end) = DateTime::parse_from_rfc3339(&alert.ends_at) else {
		return false;
	};
	alert_start.with_timezone(&Utc) <= end && alert_end.with_timezone(&Utc) >= start
}

fn collect_history(
	args: &PostmortemArgs,
	start: DateTime<Utc>,
	end: DateTime<Utc>,
) -> Result<IncidentHistory> {
	let mut history = IncidentHistory::default();
	if let Some(url) = args.alertmanager_url.as_deref().filter(|url| !url.trim().is_empty()) {
		let endpoint = format!("{}/api/v2/alerts", url.trim_end_matches('/'));
		match curl_json::<Vec<Alert>>(&endpoint, args.alertmanager_token.as_deref(), false) {
			Ok(alerts) => {
				history.alerts = alerts
					.into_iter()
					.filter(|alert| alert_overlaps_window(alert, start, end))
					.collect();
				history.alerts.sort_by_key(|alert| {
					DateTime::parse_from_rfc3339(&alert.starts_at)
						.map(|time| time.timestamp_millis())
						.unwrap_or_default()
				});
			}
			Err(error) => history.source_notes.push(format!("Alert history unavailable: {error}")),
		}
	} else {
		history.source_notes.push("Alert history not configured (set ALERTMANAGER_URL).".into());
	}

	if let Some(url) = args.prometheus_url.as_deref().filter(|url| !url.trim().is_empty()) {
		for (name, expression) in parse_metric_queries(&args.metric_queries)? {
			match query_prometheus(
				url,
				args.prometheus_token.as_deref(),
				name.clone(),
				expression.clone(),
				start.timestamp(),
				end.timestamp(),
				args.step_secs,
			) {
				Ok(metric) => history.metrics.push(metric),
				Err(error) => history.source_notes.push(format!("Metric query {name:?} unavailable: {error}")),
			}
		}
	} else {
		history.source_notes.push("Metric snapshots not configured (set PROMETHEUS_URL).".into());
	}

	if let Some(repo) = args.github_repo.as_deref().filter(|repo| !repo.trim().is_empty()) {
		let api_base = format!("https://api.github.com/repos/{repo}");
		let releases_url = format!("{api_base}/releases?per_page=100");
		match curl_json::<Vec<Release>>(&releases_url, args.github_token.as_deref(), true) {
			Ok(releases) => {
				history.releases = releases
					.into_iter()
					.filter(|release| release.published_at.as_deref().is_some_and(|time| time_in_window(time, start, end)))
					.collect();
				history.releases.sort_by_key(|release| {
					release.published_at.as_deref()
						.and_then(|time| DateTime::parse_from_rfc3339(time).ok())
						.map(|time| time.timestamp_millis())
						.unwrap_or_default()
				});
			}
			Err(error) => history.source_notes.push(format!("GitHub release history unavailable: {error}")),
		}
		let deployments_url = format!("{api_base}/deployments?per_page=100");
		match curl_json::<Vec<Deployment>>(&deployments_url, args.github_token.as_deref(), true) {
			Ok(deployments) => {
				history.deployments = deployments
					.into_iter()
					.filter(|deployment| time_in_window(&deployment.created_at, start, end))
					.collect();
				history.deployments.sort_by_key(|deployment| {
					DateTime::parse_from_rfc3339(&deployment.created_at)
						.map(|time| time.timestamp_millis())
						.unwrap_or_default()
				});
			}
			Err(error) => history.source_notes.push(format!("GitHub deployment history unavailable: {error}")),
		}
	} else {
		history.source_notes.push("Release and deployment history not configured (set GITHUB_REPOSITORY).".into());
	}
	Ok(history)
}

fn markdown_cell(value: &str) -> String {
	value.replace('|', "\\|").replace('\n', " ").replace('\r', " ")
}

fn encode_query(value: &str) -> String {
	let mut encoded = String::new();
	for byte in value.bytes() {
		if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
			encoded.push(byte as char);
		} else {
			encoded.push_str(&format!("%{byte:02X}"));
		}
	}
	encoded
}

fn render_markdown(
	start: DateTime<Utc>,
	end: DateTime<Utc>,
	history: &IncidentHistory,
) -> String {
	let mut output = format!(
		"# Postmortem: [incident title]\n\n**Status:** Draft\n\n**Incident window:** {} to {} (UTC)\n\n## Summary\n\n[What happened, who was affected, and for how long?]\n\n## Impact\n\n[Customer and operational impact; quantify where possible.]\n\n## Incident Timeline\n\n",
		start.to_rfc3339(),
		end.to_rfc3339()
	);
	output.push_str("### Alerts\n\n");
	if history.alerts.is_empty() {
		output.push_str("No matching alerts were returned for this incident window. This may be a customer-reported incident or the alert backend may not retain historical alerts.\n\n");
	} else {
		output.push_str("| Started | Ended | Alert | State | Description | Source |\n|---|---|---|---|---|---|\n");
		for alert in &history.alerts {
			let name = alert.labels.get("alertname").map(String::as_str).unwrap_or("unnamed alert");
			let description = alert.annotations.get("summary").or_else(|| alert.annotations.get("description")).map(String::as_str).unwrap_or("");
			let source = if alert.generator_url.is_empty() { String::new() } else { format!("[link]({})", alert.generator_url) };
			output.push_str(&format!(
				"| {} | {} | {} | {} | {} | {} |\n",
				markdown_cell(&alert.starts_at), markdown_cell(&alert.ends_at), markdown_cell(name),
				markdown_cell(&alert.status.state), markdown_cell(description), source
			));
		}
		output.push('\n');
	}

	output.push_str("### Metrics\n\n");
	if history.metrics.is_empty() {
		output.push_str("No metric snapshots were collected. Configure Prometheus to include graph links and sampled values here.\n\n");
	} else {
		for metric in &history.metrics {
			let graph = format!("/graph?g0.expr={}&g0.tab=1", encode_query(&metric.expression));
			output.push_str(&format!("#### {}\n\nPromQL: `{}`\n\n", metric.name, metric.expression));
			if let Some(base) = history.prometheus_url.as_deref() {
				output.push_str(&format!("[Open graph]({}{})\n\n", base.trim_end_matches('/'), graph));
			}
			if metric.series.is_empty() {
				output.push_str("No samples returned.\n\n");
				continue;
			}
			output.push_str("| Time (UTC) | Series | Value |\n|---|---|---:|\n");
			for series in &metric.series {
				let labels = series.metric.iter().map(|(key, value)| format!("{key}={value}")).collect::<Vec<_>>().join(", ");
				for (timestamp, value) in &series.values {
					let time = DateTime::from_timestamp(*timestamp as i64, 0).map(|time| time.to_rfc3339()).unwrap_or_else(|| timestamp.to_string());
					output.push_str(&format!("| {} | {} | {} |\n", time, markdown_cell(&labels), markdown_cell(value)));
				}
			}
			output.push('\n');
		}
	}

	output.push_str("### Deployments and Releases\n\n");
	if history.deployments.is_empty() && history.releases.is_empty() {
		output.push_str("No deployments or releases were returned for this incident window.\n\n");
	} else {
		output.push_str("| Time (UTC) | Type | Reference | Details |\n|---|---|---|---|\n");
		for deployment in &history.deployments {
			output.push_str(&format!("| {} | Deployment #{} | {} ({}) | {} {} ({}) |\n", markdown_cell(&deployment.created_at), deployment.id, markdown_cell(&deployment.ref_name), markdown_cell(&deployment.sha), markdown_cell(&deployment.environment), markdown_cell(&deployment.task), markdown_cell(&deployment.description)));
		}
		for release in &history.releases {
			let published = release.published_at.as_deref().unwrap_or("unknown");
			let title = if release.name.is_empty() { release.tag_name.as_str() } else { release.name.as_str() };
			let detail = if release.html_url.is_empty() { title.to_owned() } else { format!("[{}]({})", markdown_cell(title), release.html_url) };
			output.push_str(&format!("| {} | Release | {} | {} |\n", markdown_cell(published), markdown_cell(&release.tag_name), detail));
		}
		output.push('\n');
	}

	output.push_str("## Root Cause\n\n[Human-authored analysis. Do not infer cause from the collected signals alone.]\n\n## Resolution\n\n[What stopped or mitigated the incident?]\n\n## Follow-up Actions\n\n| Action | Owner | Due date | Status |\n|---|---|---|---|\n| | | | |\n\n## Data Collection Notes\n\n");
	if history.source_notes.is_empty() {
		output.push_str("All configured history sources responded.\n");
	} else {
		for note in &history.source_notes {
			output.push_str(&format!("- {}\n", markdown_cell(note)));
		}
	}
	output
}

pub fn run(args: PostmortemArgs) -> Result<()> {
	if args.step_secs < 1 {
		bail!("step-secs must be positive");
	}
	let start = parse_timestamp(&args.start, "start")?;
	let end = parse_timestamp(&args.end, "end")?;
	if end < start {
		bail!("end must be equal to or later than start");
	}
	let mut history = collect_history(&args, start, end)?;
	history.prometheus_url = args
		.prometheus_url
		.as_deref()
		.filter(|url| !url.trim().is_empty())
		.map(str::to_owned);
	let markdown = render_markdown(start, end, &history);
	if let Some(parent) = args.output.parent().filter(|parent| !parent.as_os_str().is_empty()) {
		fs::create_dir_all(parent).with_context(|| format!("creating output directory {}", parent.display()))?;
	}
	fs::write(&args.output, markdown).with_context(|| format!("writing {}", args.output.display()))?;
	println!("Postmortem draft written to {}", args.output.display());
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	const ALERTS_FIXTURE: &str = r#"[
		{"labels":{"alertname":"ElevatedErrors","severity":"page"},"annotations":{"summary":"5xx rate exceeded threshold"},"startsAt":"2026-09-30T10:05:00Z","endsAt":"2026-09-30T10:25:00Z","status":{"state":"resolved"},"generatorURL":"https://prom.example/graph"}
	]"#;
	const DEPLOYMENTS_FIXTURE: &str = r#"[
		{"id":42,"sha":"abc123","ref":"main","environment":"production","task":"deploy","created_at":"2026-09-30T10:12:00Z","description":"release rollout"}
	]"#;
	const RELEASES_FIXTURE: &str = r#"[
		{"tag_name":"v1.2.3","name":"Version 1.2.3","html_url":"https://github.com/acme/app/releases/tag/v1.2.3","published_at":"2026-09-30T10:15:00Z"}
	]"#;
	const METRICS_FIXTURE: &str = r#"{"status":"success","data":{"result":[{"metric":{"service":"api"},"values":[[1790762700,"0.12"],[1790762760,"0.18"]]}]}}"#;

	#[test]
	fn parses_alert_deploy_release_and_metric_fixtures_into_draft() {
		let start = parse_timestamp("2026-09-30T10:00:00Z", "start").unwrap();
		let end = parse_timestamp("2026-09-30T10:30:00Z", "end").unwrap();
		let alerts: Vec<Alert> = serde_json::from_str(ALERTS_FIXTURE).unwrap();
		let deployments: Vec<Deployment> = serde_json::from_str(DEPLOYMENTS_FIXTURE).unwrap();
		let releases: Vec<Release> = serde_json::from_str(RELEASES_FIXTURE).unwrap();
		let response: PrometheusResponse = serde_json::from_str(METRICS_FIXTURE).unwrap();
		let history = IncidentHistory {
			alerts: alerts.into_iter().filter(|alert| time_in_window(&alert.starts_at, start, end)).collect(),
			metrics: vec![MetricSnapshot {
				name: "Error rate".into(),
				expression: "sum(rate(errors[5m]))".into(),
				series: response.data.unwrap().result,
			}],
			deployments: deployments.into_iter().filter(|item| time_in_window(&item.created_at, start, end)).collect(),
			releases: releases.into_iter().filter(|item| item.published_at.as_deref().is_some_and(|time| time_in_window(time, start, end))).collect(),
			..IncidentHistory::default()
		};
		let markdown = render_markdown(start, end, &history);
		for expected in ["ElevatedErrors", "5xx rate exceeded threshold", "0.18", "main", "v1.2.3", "Root Cause"] {
			assert!(markdown.contains(expected), "draft missing {expected:?}");
		}
	}

	#[test]
	fn no_alerts_still_generates_usable_draft() {
		let start = parse_timestamp("2026-09-30T10:00:00Z", "start").unwrap();
		let end = parse_timestamp("2026-09-30T10:30:00Z", "end").unwrap();
		let markdown = render_markdown(start, end, &IncidentHistory::default());
		assert!(markdown.contains("No matching alerts were returned"));
		assert!(markdown.contains("## Root Cause"));
		assert!(markdown.contains("## Follow-up Actions"));
	}

	#[test]
	fn alert_spanning_the_entire_window_is_included() {
		let start = parse_timestamp("2026-09-30T10:00:00Z", "start").unwrap();
		let end = parse_timestamp("2026-09-30T10:30:00Z", "end").unwrap();
		let alert: Alert = serde_json::from_str(
			r#"{"startsAt":"2026-09-30T09:00:00Z","endsAt":"2026-09-30T11:00:00Z"}"#,
		)
		.unwrap();
		assert!(alert_overlaps_window(&alert, start, end));
	}
}