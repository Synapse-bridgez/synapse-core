//! `cargo xtask scorecard` — per-release reliability scorecard.
//!
//! Compares production reliability metrics (error rate, P50/P95/P99 latency,
//! incident count) in a window before a release against an equivalent window
//! after it, and flags statistically meaningful regressions separately from
//! noise (see [`stats`]). Reporting only — nothing is rolled back.
//!
//! Run automatically by `.github/workflows/release-scorecard.yml` once a
//! release's "after" window has closed; see `docs/release-scorecard.md`.
//!
//! ## Releases close together
//!
//! With releases A then B, B's "before" window must not reach back past A's
//! deploy (that traffic ran different code), and A's "after" window must stop
//! at B's deploy. Both windows are clipped at the neighbouring release, then
//! shortened to the *same* length on both sides of the release so the
//! comparison is not skewed by time-of-day or weekday effects. The report
//! says when this happened and which release caused it; if the clipped
//! window is too short to compare, metrics come out as insufficient data
//! rather than a misleading verdict.

pub mod stats;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context};
use chrono::{DateTime, Duration, Utc};
use clap::Args;
use serde::{Deserialize, Serialize};

use stats::{Comparison, Thresholds, Verdict};

#[derive(Args)]
pub struct ScorecardArgs {
    /// Release tag to score. Mutually exclusive with --due.
    #[arg(long, conflicts_with = "due")]
    pub release: Option<String>,

    /// Score every release whose "after" window closed within the last
    /// --lookback-hours (what the scheduled workflow runs).
    #[arg(long)]
    pub due: bool,

    #[arg(long, default_value_t = 24)]
    pub lookback_hours: i64,

    /// Length of each comparison window.
    #[arg(long, default_value_t = 24)]
    pub window_hours: i64,

    /// JSON file listing deploys: `[{"tag": "v1.2.3", "deployed_at": "<RFC 3339>"}]`.
    /// Defaults to git tags matching --tag-pattern, dated by tag creation.
    #[arg(long)]
    pub releases_file: Option<PathBuf>,

    #[arg(long, default_value = "v*")]
    pub tag_pattern: String,

    #[arg(long, env = "PROMETHEUS_URL")]
    pub prometheus_url: Option<String>,

    #[arg(long, env = "PROMETHEUS_BEARER_TOKEN", hide_env_values = true)]
    pub prometheus_token: Option<String>,

    /// Scrape resolution used for range queries.
    #[arg(long, default_value_t = 300)]
    pub step_secs: i64,

    /// Samples averaged into one block before testing (12 × 5 min = 1 h).
    #[arg(long, default_value_t = 12)]
    pub block_size: usize,

    #[arg(long, default_value_t = 0.01)]
    pub alpha: f64,

    #[arg(long, default_value_t = 0.10)]
    pub min_relative_change: f64,

    /// Where `<tag>.md`, `<tag>.json` and `summary.json` are written.
    #[arg(long, default_value = "scorecard")]
    pub out_dir: PathBuf,

    /// Exit non-zero if any scored release has a regression.
    #[arg(long)]
    pub fail_on_regression: bool,
}

// ---------------------------------------------------------------------------
// Releases and windows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Release {
    pub tag: String,
    pub deployed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Window {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl Window {
    pub fn hours(&self) -> f64 {
        (self.end - self.start).num_seconds() as f64 / 3600.0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Windows {
    pub before: Window,
    pub after: Window,
    /// Previous release that clipped the "before" window.
    pub clipped_by_previous: Option<String>,
    /// Next release that clipped the "after" window.
    pub clipped_by_next: Option<String>,
    /// The "after" window has not fully elapsed yet.
    pub incomplete: bool,
}

impl Windows {
    pub fn clipped(&self) -> bool {
        self.clipped_by_previous.is_some() || self.clipped_by_next.is_some() || self.incomplete
    }
}

/// Comparison windows for `releases[idx]` (releases sorted by deploy time).
pub fn compute_windows(
    releases: &[Release],
    idx: usize,
    window: Duration,
    now: DateTime<Utc>,
) -> Windows {
    let release = &releases[idx];
    let at = release.deployed_at;
    let prev = idx.checked_sub(1).map(|i| &releases[i]);
    let next = releases.get(idx + 1);

    let mut before_len = window;
    let mut clipped_by_previous = None;
    if let Some(p) = prev.filter(|p| at - p.deployed_at < window) {
        before_len = (at - p.deployed_at).max(Duration::zero());
        clipped_by_previous = Some(p.tag.clone());
    }

    let mut after_len = window;
    let mut clipped_by_next = None;
    if let Some(n) = next.filter(|n| n.deployed_at - at < window) {
        after_len = (n.deployed_at - at).max(Duration::zero());
        clipped_by_next = Some(n.tag.clone());
    }
    let incomplete = now < at + after_len;
    if incomplete {
        after_len = (now - at).max(Duration::zero());
    }

    let len = before_len.min(after_len);
    Windows {
        before: Window {
            start: at - len,
            end: at,
        },
        after: Window {
            start: at,
            end: at + len,
        },
        clipped_by_previous,
        clipped_by_next,
        incomplete,
    }
}

/// When the "after" window of `releases[idx]` closes (window end, or the
/// next deploy if sooner).
pub fn after_window_closes(releases: &[Release], idx: usize, window: Duration) -> DateTime<Utc> {
    let end = releases[idx].deployed_at + window;
    match releases.get(idx + 1) {
        Some(n) if n.deployed_at < end => n.deployed_at,
        _ => end,
    }
}

/// Indices of releases whose "after" window closed in `(now - lookback, now]`.
pub fn due_releases(
    releases: &[Release],
    window: Duration,
    lookback: Duration,
    now: DateTime<Utc>,
) -> Vec<usize> {
    (0..releases.len())
        .filter(|i| {
            let closes = after_window_closes(releases, *i, window);
            closes > now - lookback && closes <= now
        })
        .collect()
}

fn load_releases(args: &ScorecardArgs) -> anyhow::Result<Vec<Release>> {
    let mut releases: Vec<Release> = match &args.releases_file {
        Some(path) => serde_json::from_str(
            &std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?,
        )
        .with_context(|| format!("parsing {}", path.display()))?,
        None => {
            let out = Command::new("git")
                .args([
                    "for-each-ref",
                    "--format=%(refname:short)|%(creatordate:iso-strict)",
                    &format!("refs/tags/{}", args.tag_pattern),
                ])
                .output()
                .context("running git for-each-ref")?;
            if !out.status.success() {
                bail!(
                    "git for-each-ref failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            parse_git_tags(&String::from_utf8_lossy(&out.stdout))?
        }
    };
    releases.sort_by_key(|r| r.deployed_at);
    Ok(releases)
}

pub fn parse_git_tags(output: &str) -> anyhow::Result<Vec<Release>> {
    output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let (tag, date) = line
                .split_once('|')
                .with_context(|| format!("unexpected git output line {line:?}"))?;
            Ok(Release {
                tag: tag.to_string(),
                deployed_at: DateTime::parse_from_rfc3339(date.trim())
                    .with_context(|| format!("bad date for tag {tag}: {date}"))?
                    .with_timezone(&Utc),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Metric source
// ---------------------------------------------------------------------------

/// One time series from a range query: `(unix seconds, value)` points.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Series {
    pub labels: BTreeMap<String, String>,
    pub points: Vec<(i64, f64)>,
}

pub trait MetricSource {
    fn range(&self, query: &str, window: &Window, step_secs: i64) -> anyhow::Result<Vec<Series>>;
}

/// Prometheus `query_range` over HTTP, via `curl` (present on every CI
/// runner) so xtask needs no HTTP client dependency.
pub struct Prometheus {
    pub url: String,
    pub token: Option<String>,
}

impl MetricSource for Prometheus {
    fn range(&self, query: &str, window: &Window, step_secs: i64) -> anyhow::Result<Vec<Series>> {
        let mut cmd = Command::new("curl");
        cmd.args(["-fsS", "--get", "--max-time", "60"])
            .arg("--data-urlencode")
            .arg(format!("query={query}"))
            .arg("--data-urlencode")
            .arg(format!("start={}", window.start.timestamp()))
            .arg("--data-urlencode")
            .arg(format!("end={}", window.end.timestamp()))
            .arg("--data-urlencode")
            .arg(format!("step={step_secs}"));
        if let Some(token) = &self.token {
            cmd.arg("-H").arg(format!("Authorization: Bearer {token}"));
        }
        cmd.arg(format!(
            "{}/api/v1/query_range",
            self.url.trim_end_matches('/')
        ));
        let out = cmd.output().context("running curl")?;
        if !out.status.success() {
            bail!(
                "Prometheus query failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        parse_prometheus_matrix(&String::from_utf8_lossy(&out.stdout))
    }
}

pub fn parse_prometheus_matrix(body: &str) -> anyhow::Result<Vec<Series>> {
    #[derive(Deserialize)]
    struct Resp {
        status: String,
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        data: Option<Data>,
    }
    #[derive(Deserialize)]
    struct Data {
        result: Vec<Item>,
    }
    #[derive(Deserialize)]
    struct Item {
        #[serde(default)]
        metric: BTreeMap<String, String>,
        values: Vec<(f64, String)>,
    }

    let resp: Resp = serde_json::from_str(body).context("parsing Prometheus response")?;
    if resp.status != "success" {
        bail!("Prometheus error: {}", resp.error.unwrap_or_default());
    }
    Ok(resp
        .data
        .map(|d| d.result)
        .unwrap_or_default()
        .into_iter()
        .map(|item| Series {
            labels: item.metric,
            points: item
                .values
                .into_iter()
                .filter_map(|(ts, v)| {
                    v.parse::<f64>()
                        .ok()
                        .filter(|v| v.is_finite())
                        .map(|v| (ts as i64, v))
                })
                .collect(),
        })
        .collect())
}

/// Number of distinct firing episodes across `series` (one per alert label
/// set): a gap of more than one step between points starts a new episode.
pub fn count_episodes(series: &[Series], step_secs: i64) -> u64 {
    series
        .iter()
        .map(|s| {
            let mut episodes = 0;
            let mut last: Option<i64> = None;
            for (ts, _) in &s.points {
                if last.is_none_or(|l| ts - l > step_secs) {
                    episodes += 1;
                }
                last = Some(*ts);
            }
            episodes
        })
        .sum()
}

// ---------------------------------------------------------------------------
// Scorecard
// ---------------------------------------------------------------------------

/// A continuous, higher-is-worse metric and how to query it.
pub struct MetricDef {
    pub key: &'static str,
    pub name: &'static str,
    pub unit: &'static str,
    pub query: &'static str,
    /// Changes smaller than this are never flagged, however significant.
    pub min_absolute_change: f64,
}

pub const METRICS: &[MetricDef] = &[
    MetricDef {
        key: "error_rate",
        name: "Error rate (5xx)",
        unit: "%",
        query: r#"100 * sum(rate(http_request_duration_ms_count{status=~"5.."}[5m])) / sum(rate(http_request_duration_ms_count[5m]))"#,
        min_absolute_change: 0.1,
    },
    MetricDef {
        key: "latency_p50_ms",
        name: "Latency P50",
        unit: "ms",
        query: "histogram_quantile(0.50, sum by (le) (rate(http_request_duration_ms_bucket[5m])))",
        min_absolute_change: 5.0,
    },
    MetricDef {
        key: "latency_p95_ms",
        name: "Latency P95",
        unit: "ms",
        query: "histogram_quantile(0.95, sum by (le) (rate(http_request_duration_ms_bucket[5m])))",
        min_absolute_change: 10.0,
    },
    MetricDef {
        key: "latency_p99_ms",
        name: "Latency P99",
        unit: "ms",
        query: "histogram_quantile(0.99, sum by (le) (rate(http_request_duration_ms_bucket[5m])))",
        min_absolute_change: 20.0,
    },
];

/// Firing alerts, excluding the always-on dead-man's switch.
pub const INCIDENT_QUERY: &str =
    r#"ALERTS{alertstate="firing", alertname!="Watchdog", severity=~"warning|critical"}"#;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetricResult {
    pub key: String,
    pub name: String,
    pub unit: String,
    #[serde(flatten)]
    pub comparison: Comparison,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Scorecard {
    pub release: Release,
    pub windows: Windows,
    pub metrics: Vec<MetricResult>,
    pub generated_at: DateTime<Utc>,
}

impl Scorecard {
    pub fn has_regression(&self) -> bool {
        self.metrics
            .iter()
            .any(|m| m.comparison.verdict == Verdict::Regression)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ScoreConfig {
    pub window: Duration,
    pub step_secs: i64,
    pub block_size: usize,
    pub alpha: f64,
    pub min_relative_change: f64,
}

fn values(series: &[Series]) -> Vec<f64> {
    // Aggregate queries return one series; take the first.
    series
        .first()
        .map(|s| s.points.iter().map(|p| p.1).collect())
        .unwrap_or_default()
}

pub fn score_release(
    source: &dyn MetricSource,
    releases: &[Release],
    idx: usize,
    cfg: ScoreConfig,
    now: DateTime<Utc>,
) -> anyhow::Result<Scorecard> {
    let windows = compute_windows(releases, idx, cfg.window, now);
    // At least a quarter of the configured window, and 3 blocks, per side.
    let window_blocks =
        (cfg.window.num_seconds() / cfg.step_secs.max(1)) as usize / cfg.block_size.max(1);
    let min_blocks = (window_blocks / 4).max(3);

    let mut metrics = Vec::new();
    for def in METRICS {
        let before = stats::block_means(
            &values(&source.range(def.query, &windows.before, cfg.step_secs)?),
            cfg.block_size,
        );
        let after = stats::block_means(
            &values(&source.range(def.query, &windows.after, cfg.step_secs)?),
            cfg.block_size,
        );
        let comparison = stats::compare_continuous(
            &before,
            &after,
            Thresholds {
                alpha: cfg.alpha,
                min_relative_change: cfg.min_relative_change,
                min_absolute_change: def.min_absolute_change,
                min_blocks,
            },
        );
        metrics.push(MetricResult {
            key: def.key.into(),
            name: def.name.into(),
            unit: def.unit.into(),
            comparison,
        });
    }

    let incidents_before = count_episodes(
        &source.range(INCIDENT_QUERY, &windows.before, cfg.step_secs)?,
        cfg.step_secs,
    );
    let incidents_after = count_episodes(
        &source.range(INCIDENT_QUERY, &windows.after, cfg.step_secs)?,
        cfg.step_secs,
    );
    let mut incidents = stats::compare_counts(
        incidents_before,
        windows.before.hours(),
        incidents_after,
        windows.after.hours(),
        cfg.alpha,
    );
    if windows.before.hours() * 4.0 < cfg.window.num_seconds() as f64 / 3600.0 {
        incidents.verdict = Verdict::InsufficientData;
    }
    metrics.push(MetricResult {
        key: "incidents".into(),
        name: "Incidents (alert firings)".into(),
        unit: "count".into(),
        comparison: incidents,
    });

    Ok(Scorecard {
        release: releases[idx].clone(),
        windows,
        metrics,
        generated_at: now,
    })
}

fn fmt_value(v: Option<f64>, unit: &str) -> String {
    match v {
        None => "—".into(),
        Some(v) if unit == "count" => format!("{v:.0}"),
        Some(v) if unit == "%" => format!("{v:.3}%"),
        Some(v) => format!("{v:.1} {unit}"),
    }
}

pub fn render_markdown(card: &Scorecard) -> String {
    let w = &card.windows;
    let mut md = format!(
        "## Reliability scorecard: `{}`\n\nDeployed {} · before `{}` → `{}` · after `{}` → `{}` ({:.1} h each)\n\n",
        card.release.tag,
        card.release.deployed_at.to_rfc3339(),
        w.before.start.to_rfc3339(),
        w.before.end.to_rfc3339(),
        w.after.start.to_rfc3339(),
        w.after.end.to_rfc3339(),
        w.after.hours(),
    );
    if let Some(prev) = &w.clipped_by_previous {
        md += &format!(
            "> ⚠️ The *before* window was clipped at the previous release `{prev}`; both windows were shortened to match. Attribute regressions to this release with care.\n\n"
        );
    }
    if let Some(next) = &w.clipped_by_next {
        md += &format!(
            "> ⚠️ The *after* window was clipped at the next release `{next}`; both windows were shortened to match.\n\n"
        );
    }
    if w.incomplete {
        md += "> ⚠️ The *after* window has not fully elapsed yet; this is a partial scorecard.\n\n";
    }
    md +=
        "| Metric | Before | After | Change | p-value | Verdict |\n|---|---:|---:|---:|---:|---|\n";
    for m in &card.metrics {
        let c = &m.comparison;
        let change = match c.relative_change {
            Some(r) if r.is_infinite() => "new".into(),
            Some(r) => format!("{:+.1}%", r * 100.0),
            None => "—".into(),
        };
        let p = c.p_value.map_or("—".into(), |p| format!("{p:.4}"));
        md += &format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            m.name,
            fmt_value(c.before, &m.unit),
            fmt_value(c.after, &m.unit),
            change,
            p,
            c.verdict.label()
        );
    }
    md += "\nBefore/after values are medians of hourly block means (incidents: totals). \
           A regression needs p below alpha **and** a change above the minimum effect size; \
           see `docs/release-scorecard.md`.\n";
    md
}

fn write_outputs(out_dir: &Path, cards: &[Scorecard]) -> anyhow::Result<()> {
    std::fs::create_dir_all(out_dir)?;
    for card in cards {
        let md = render_markdown(card);
        let safe = card.release.tag.replace(['/', '\\'], "_");
        std::fs::write(out_dir.join(format!("{safe}.md")), &md)?;
        std::fs::write(
            out_dir.join(format!("{safe}.json")),
            serde_json::to_string_pretty(card)?,
        )?;
        println!("{md}");
        if let Ok(summary) = std::env::var("GITHUB_STEP_SUMMARY") {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(summary)?;
            writeln!(f, "{md}")?;
        }
    }
    let summary = serde_json::json!({
        "scored": cards.iter().map(|c| &c.release.tag).collect::<Vec<_>>(),
        "regressions": cards.iter().filter(|c| c.has_regression()).map(|c| &c.release.tag).collect::<Vec<_>>(),
        "clipped": cards.iter().filter(|c| c.windows.clipped()).map(|c| &c.release.tag).collect::<Vec<_>>(),
    });
    std::fs::write(
        out_dir.join("summary.json"),
        serde_json::to_string_pretty(&summary)?,
    )?;
    Ok(())
}

pub fn run(args: ScorecardArgs) -> anyhow::Result<()> {
    let releases = load_releases(&args)?;
    let now = Utc::now();
    let window = Duration::hours(args.window_hours);

    let indices = if args.due {
        due_releases(&releases, window, Duration::hours(args.lookback_hours), now)
    } else if let Some(tag) = &args.release {
        vec![releases
            .iter()
            .position(|r| &r.tag == tag)
            .with_context(|| format!("release {tag} not found"))?]
    } else {
        bail!("pass --release <tag> or --due");
    };
    if indices.is_empty() {
        println!(
            "No release has a comparison window that closed in the last {} h.",
            args.lookback_hours
        );
        write_outputs(&args.out_dir, &[])?;
        return Ok(());
    }

    let source = Prometheus {
        url: args
            .prometheus_url
            .clone()
            .context("PROMETHEUS_URL / --prometheus-url is required")?,
        token: args.prometheus_token.clone(),
    };
    let cfg = ScoreConfig {
        window,
        step_secs: args.step_secs,
        block_size: args.block_size,
        alpha: args.alpha,
        min_relative_change: args.min_relative_change,
    };
    let cards = indices
        .into_iter()
        .map(|i| score_release(&source, &releases, i, cfg, now))
        .collect::<anyhow::Result<Vec<_>>>()?;
    write_outputs(&args.out_dir, &cards)?;

    if args.fail_on_regression && cards.iter().any(Scorecard::has_regression) {
        bail!("reliability regression detected");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(h: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + Duration::hours(h)
    }

    fn rel(tag: &str, h: i64) -> Release {
        Release {
            tag: tag.into(),
            deployed_at: t(h),
        }
    }

    #[test]
    fn isolated_release_gets_full_windows() {
        let r = vec![rel("v1", 0), rel("v2", 100), rel("v3", 200)];
        let w = compute_windows(&r, 1, Duration::hours(24), t(1000));
        assert_eq!(
            w.before,
            Window {
                start: t(76),
                end: t(100)
            }
        );
        assert_eq!(
            w.after,
            Window {
                start: t(100),
                end: t(124)
            }
        );
        assert!(!w.clipped());
    }

    #[test]
    fn close_releases_clip_both_sides_symmetrically() {
        // v2 six hours after v1, v3 ten hours after v2.
        let r = vec![rel("v1", 0), rel("v2", 6), rel("v3", 16)];
        let w = compute_windows(&r, 1, Duration::hours(24), t(1000));
        // Before is clipped at v1 (6 h); after at v3 (10 h); both use 6 h.
        assert_eq!(
            w.before,
            Window {
                start: t(0),
                end: t(6)
            }
        );
        assert_eq!(
            w.after,
            Window {
                start: t(6),
                end: t(12)
            }
        );
        assert_eq!(w.clipped_by_previous.as_deref(), Some("v1"));
        assert_eq!(w.clipped_by_next.as_deref(), Some("v3"));
        assert!(w.clipped());

        // v1's after window stops at v2 — it never includes v2's traffic.
        let w1 = compute_windows(&r, 0, Duration::hours(24), t(1000));
        assert_eq!(w1.after.end, t(6));
        assert_eq!(w1.before.start, t(-6));
        // …and v2's before window starts no earlier than v1's deploy.
        assert!(w.before.start >= r[0].deployed_at);
    }

    #[test]
    fn incomplete_after_window() {
        let r = vec![rel("v1", 0)];
        let w = compute_windows(&r, 0, Duration::hours(24), t(5));
        assert!(w.incomplete);
        assert_eq!(w.after.end, t(5));
        assert_eq!(w.before.start, t(-5));
    }

    #[test]
    fn due_releases_by_window_close() {
        let r = vec![rel("v1", 0), rel("v2", 30), rel("v3", 34)];
        let day = Duration::hours(24);
        // v1 closes at 24, v2 at 34 (clipped by v3), v3 at 58.
        assert_eq!(due_releases(&r, day, day, t(25)), vec![0]);
        assert_eq!(due_releases(&r, day, day, t(40)), vec![0, 1]);
        assert_eq!(due_releases(&r, day, day, t(57)), vec![1]);
        assert_eq!(due_releases(&r, day, day, t(58)), vec![2]);
        assert_eq!(due_releases(&r, day, day, t(100)), Vec::<usize>::new());
        assert_eq!(after_window_closes(&r, 1, day), t(34));
    }

    #[test]
    fn parses_git_tags_and_prometheus() {
        let rs =
            parse_git_tags("v1.0.0|2026-09-01T10:00:00+02:00\n\nv1.1.0|2026-09-03T08:00:00Z\n")
                .unwrap();
        assert_eq!(rs.len(), 2);
        assert_eq!(rs[0].deployed_at, t(8));
        assert!(parse_git_tags("garbage").is_err());
        assert!(parse_git_tags("v1|not-a-date").is_err());

        let body = r#"{"status":"success","data":{"resultType":"matrix","result":[
            {"metric":{"alertname":"HighErrorRate"},"values":[[1000,"1"],[1300,"NaN"],[1600,"2.5"]]}]}}"#;
        let s = parse_prometheus_matrix(body).unwrap();
        assert_eq!(s[0].points, vec![(1000, 1.0), (1600, 2.5)]);
        assert_eq!(s[0].labels["alertname"], "HighErrorRate");
        assert!(parse_prometheus_matrix(r#"{"status":"error","error":"bad query"}"#).is_err());
        assert!(parse_prometheus_matrix(r#"{"status":"success"}"#)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn counts_alert_episodes() {
        let s = |pts: &[i64]| Series {
            labels: BTreeMap::new(),
            points: pts.iter().map(|p| (*p, 1.0)).collect(),
        };
        // Two episodes (gap at 900→1800) in one series, one in another.
        let series = vec![s(&[0, 300, 600, 900, 1800, 2100]), s(&[600])];
        assert_eq!(count_episodes(&series, 300), 3);
        assert_eq!(count_episodes(&[], 300), 0);
    }

    /// Synthetic Prometheus: per-query value generators by time.
    struct Synthetic {
        release_ts: i64,
        p95_after_factor: f64,
        incidents_after: usize,
    }

    impl MetricSource for Synthetic {
        fn range(&self, query: &str, w: &Window, step: i64) -> anyhow::Result<Vec<Series>> {
            let (start, end) = (w.start.timestamp(), w.end.timestamp());
            if query == INCIDENT_QUERY {
                let n = if start >= self.release_ts {
                    self.incidents_after
                } else {
                    1
                };
                return Ok((0..n)
                    .map(|i| Series {
                        labels: BTreeMap::new(),
                        points: vec![(start + i as i64 * 3600, 1.0)],
                    })
                    .collect());
            }
            let base = if query.contains("0.95") {
                200.0
            } else if query.contains("0.99") {
                400.0
            } else if query.contains("0.50") {
                50.0
            } else {
                0.5
            };
            let points = (start..end)
                .step_by(step as usize)
                .enumerate()
                .map(|(i, ts)| {
                    // Daily cycle + deterministic jitter: realistic noise.
                    let hour = (ts / 3600 % 24) as f64;
                    let cycle = 1.0 + 0.2 * (hour / 24.0 * std::f64::consts::TAU).sin();
                    let jitter = 1.0 + 0.05 * (((i * 7919) % 13) as f64 / 6.0 - 1.0);
                    let factor = if ts >= self.release_ts && query.contains("0.95") {
                        self.p95_after_factor
                    } else {
                        1.0
                    };
                    (ts, base * cycle * jitter * factor)
                })
                .collect();
            Ok(vec![Series {
                labels: BTreeMap::new(),
                points,
            }])
        }
    }

    const CFG: ScoreConfig = ScoreConfig {
        window: Duration::hours(24),
        step_secs: 300,
        block_size: 12,
        alpha: 0.01,
        min_relative_change: 0.10,
    };

    #[test]
    fn scorecard_flags_known_regression_only() {
        let r = vec![rel("v1", 0), rel("v2", 100)];
        let src = Synthetic {
            release_ts: t(100).timestamp(),
            p95_after_factor: 1.4,
            incidents_after: 1,
        };
        let card = score_release(&src, &r, 1, CFG, t(1000)).unwrap();
        let verdict = |k: &str| {
            card.metrics
                .iter()
                .find(|m| m.key == k)
                .unwrap()
                .comparison
                .verdict
        };
        assert_eq!(verdict("latency_p95_ms"), Verdict::Regression);
        assert_eq!(verdict("latency_p50_ms"), Verdict::NoSignificantChange);
        assert_eq!(verdict("latency_p99_ms"), Verdict::NoSignificantChange);
        assert_eq!(verdict("error_rate"), Verdict::NoSignificantChange);
        assert_eq!(verdict("incidents"), Verdict::NoSignificantChange);
        assert!(card.has_regression());

        let md = render_markdown(&card);
        assert!(md.contains("`v2`"));
        assert!(md.contains("| Latency P95 |"));
        assert!(md.contains("regression"));
        assert!(!md.contains("clipped"));
    }

    #[test]
    fn scorecard_noise_only_release_is_clean() {
        let r = vec![rel("v1", 0), rel("v2", 100)];
        let src = Synthetic {
            release_ts: t(100).timestamp(),
            p95_after_factor: 1.0,
            incidents_after: 1,
        };
        let card = score_release(&src, &r, 1, CFG, t(1000)).unwrap();
        assert!(!card.has_regression(), "{card:#?}");
    }

    #[test]
    fn scorecard_incident_spike_and_clipped_windows() {
        let r = vec![rel("v1", 0), rel("v2", 20), rel("v3", 200)];
        let src = Synthetic {
            release_ts: t(20).timestamp(),
            p95_after_factor: 1.0,
            incidents_after: 15,
        };
        let card = score_release(&src, &r, 1, CFG, t(1000)).unwrap();
        let incidents = card.metrics.iter().find(|m| m.key == "incidents").unwrap();
        assert_eq!(incidents.comparison.verdict, Verdict::Regression);
        let md = render_markdown(&card);
        assert!(md.contains("clipped at the previous release `v1`"));

        // Very close releases: too little data on either side to judge.
        let r = vec![rel("v1", 0), rel("v2", 2), rel("v3", 200)];
        let card = score_release(&src, &r, 1, CFG, t(1000)).unwrap();
        assert!(
            card.metrics
                .iter()
                .all(|m| m.comparison.verdict == Verdict::InsufficientData),
            "{card:#?}"
        );
    }

    #[test]
    fn writes_report_files() {
        let dir = std::env::temp_dir().join(format!("scorecard-test-{}", std::process::id()));
        let r = vec![rel("release/v2", 100)];
        let src = Synthetic {
            release_ts: t(100).timestamp(),
            p95_after_factor: 1.0,
            incidents_after: 0,
        };
        let card = score_release(&src, &r, 0, CFG, t(110)).unwrap();
        assert!(card.windows.incomplete);
        assert!(render_markdown(&card).contains("partial scorecard"));
        write_outputs(&dir, &[card]).unwrap();
        assert!(dir.join("release_v2.md").exists());
        assert!(dir.join("release_v2.json").exists());
        let summary: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("summary.json")).unwrap())
                .unwrap();
        assert_eq!(summary["scored"][0], "release/v2");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn value_formatting() {
        assert_eq!(fmt_value(None, "ms"), "—");
        assert_eq!(fmt_value(Some(3.0), "count"), "3");
        assert_eq!(fmt_value(Some(0.5), "%"), "0.500%");
        assert_eq!(fmt_value(Some(12.34), "ms"), "12.3 ms");
    }
}
