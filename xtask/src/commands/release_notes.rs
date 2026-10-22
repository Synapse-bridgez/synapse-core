use anyhow::{Context, Result};
use chrono::NaiveDate;
use clap::Args;
use serde::Deserialize;
use std::{collections::BTreeMap, fs, path::{Path, PathBuf}};

const CATEGORIES: &[&str] = &["Backend", "Database", "DevOps", "Security", "Observability", "SDK-CLI", "Governance"];
const COMPLEXITIES: &[&str] = &["High", "Medium", "Low"];
const CLOSING_KEYWORDS: &[&str] = &["close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved"];

#[derive(Args)]
pub struct ReleaseNotesArgs {
	/// Merged PRs as JSON, e.g. `gh pr list --state merged --json number,title,body,mergedAt,author`.
	#[arg(long)]
	prs: PathBuf,

	/// Issues as JSON, e.g. `gh issue list --state all --json number,body`.
	#[arg(long)]
	issues: PathBuf,

	/// First merge date to include (YYYY-MM-DD, inclusive).
	#[arg(long)]
	since: Option<String>,

	/// Last merge date to include (YYYY-MM-DD, inclusive).
	#[arg(long)]
	until: Option<String>,

	/// Output markdown path; prints to stdout when omitted.
	#[arg(long)]
	output: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct MergedPr {
	number: u64,
	#[serde(default)]
	title: String,
	#[serde(default)]
	body: String,
	#[serde(rename = "mergedAt")]
	merged_at: Option<String>,
	author: Option<Author>,
}

#[derive(Debug, Deserialize)]
struct Author {
	login: String,
}

#[derive(Debug, Deserialize)]
struct Issue {
	number: u64,
	#[serde(default)]
	body: String,
}

struct Entry {
	category: &'static str,
	complexity: &'static str,
	number: u64,
	title: String,
	author: String,
	issues: Vec<u64>,
}

struct Manual {
	number: u64,
	title: String,
	author: String,
	reason: String,
}

#[derive(Default)]
struct Report {
	entries: Vec<Entry>,
	manual: Vec<Manual>,
}

pub fn run(args: ReleaseNotesArgs) -> Result<()> {
	let since = args.since.as_deref().map(validate_date).transpose()?;
	let until = args.until.as_deref().map(validate_date).transpose()?;
	let prs: Vec<MergedPr> = read_json(&args.prs)?;
	let issues: BTreeMap<u64, Issue> = read_json::<Vec<Issue>>(&args.issues)?
		.into_iter()
		.map(|issue| (issue.number, issue))
		.collect();

	let report = build_report(&prs, &issues, since.as_deref(), until.as_deref());
	let markdown = render_markdown(&report, since.as_deref(), until.as_deref());
	match args.output {
		Some(path) => fs::write(&path, markdown).with_context(|| format!("writing {}", path.display()))?,
		None => print!("{markdown}"),
	}
	Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
	let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
	serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

fn validate_date(value: &str) -> Result<String> {
	NaiveDate::parse_from_str(value, "%Y-%m-%d")
		.with_context(|| format!("invalid date '{value}', expected YYYY-MM-DD"))?;
	Ok(value.to_string())
}

fn in_window(merged_at: &str, since: Option<&str>, until: Option<&str>) -> bool {
	let day = merged_at.get(..10).unwrap_or(merged_at);
	since.map_or(true, |s| day >= s) && until.map_or(true, |u| day <= u)
}

/// Groups merged PRs by the Wave category and complexity of their linked issues.
///
/// PRs that link no Wave issue are skipped (standard changelog covers them). A PR
/// whose linked issue is missing, or whose Wave fields don't parse, goes to the
/// manual-inclusion list instead of being dropped.
fn build_report(
	prs: &[MergedPr],
	issues: &BTreeMap<u64, Issue>,
	since: Option<&str>,
	until: Option<&str>,
) -> Report {
	let mut report = Report::default();
	for pr in prs {
		let Some(merged_at) = pr.merged_at.as_deref() else { continue };
		if !in_window(merged_at, since, until) {
			continue;
		}

		let author = pr.author.as_ref().map_or("unknown", |a| a.login.as_str()).to_string();
		let mut waves = Vec::new();
		let mut problems = Vec::new();
		for number in closing_references(&format!("{}\n{}", pr.title, pr.body)) {
			match issues.get(&number) {
				None => problems.push(format!("issue #{number} metadata unavailable")),
				Some(issue) => match classify_issue(&issue.body) {
					Ok(Some((category, complexity))) => waves.push((number, category, complexity)),
					Ok(None) => {}
					Err(reason) => problems.push(format!("issue #{number}: {reason}")),
				},
			}
		}

		if !problems.is_empty() {
			report.manual.push(Manual {
				number: pr.number,
				title: pr.title.clone(),
				author,
				reason: problems.join("; "),
			});
			continue;
		}
		let Some(&(_, category, complexity)) = waves.first() else { continue };
		report.entries.push(Entry {
			category,
			complexity,
			number: pr.number,
			title: pr.title.clone(),
			author,
			issues: waves.iter().map(|(number, _, _)| *number).collect(),
		});
	}
	report
}

/// Returns `Ok(None)` for issues that aren't Wave issues (no Wave fields at all),
/// and `Err` for issues that carry Wave fields but don't match the template.
fn classify_issue(body: &str) -> Result<Option<(&'static str, &'static str)>, String> {
	let category = field_value(body, "Category");
	let complexity = field_value(body, "Complexity");
	if category.is_none() && complexity.is_none() {
		return Ok(None);
	}

	let category = category.ok_or("missing **Category:** field")?;
	let category = canonical(&category, CATEGORIES).ok_or_else(|| format!("unknown category '{category}'"))?;

	let complexity = complexity.ok_or("missing **Complexity:** field")?;
	// Template values look like "High (200 points)"; only the first word is the level.
	let level = complexity.split_whitespace().next().unwrap_or("");
	let complexity = canonical(level, COMPLEXITIES).ok_or_else(|| format!("unknown complexity '{level}'"))?;

	Ok(Some((category, complexity)))
}

fn field_value(body: &str, label: &str) -> Option<String> {
	let marker = format!("**{label}:**");
	body.lines()
		.find_map(|line| line.trim().strip_prefix(marker.as_str()))
		.map(|value| value.trim().to_string())
}

fn canonical(value: &str, known: &[&'static str]) -> Option<&'static str> {
	known.iter().copied().find(|candidate| candidate.eq_ignore_ascii_case(value))
}

/// Extracts issue numbers from closing keywords such as `Closes #12` or `fixes: #34`.
fn closing_references(text: &str) -> Vec<u64> {
	let words: Vec<&str> = text
		.split(|c: char| c.is_whitespace() || c == ':')
		.filter(|word| !word.is_empty())
		.collect();

	let mut numbers = Vec::new();
	for pair in words.windows(2) {
		let keyword = pair[0].to_ascii_lowercase();
		if !CLOSING_KEYWORDS.contains(&keyword.as_str()) {
			continue;
		}
		let Some(rest) = pair[1].strip_prefix('#') else { continue };
		let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
		if let Ok(number) = digits.parse::<u64>() {
			if !numbers.contains(&number) {
				numbers.push(number);
			}
		}
	}
	numbers
}

fn render_markdown(report: &Report, since: Option<&str>, until: Option<&str>) -> String {
	let period = match (since, until) {
		(None, None) => "all merged changes".to_string(),
		(since, until) => format!("merged {} to {}", since.unwrap_or("the beginning"), until.unwrap_or("the present")),
	};

	let mut out = String::from("# Release Notes: Wave Contributions\n\n");
	out.push_str(&format!(
		"_Covers {period}. Only PRs linked to Wave issues are listed._\n\n"
	));

	if report.entries.is_empty() && report.manual.is_empty() {
		out.push_str("_No merged Wave-linked changes in this window._\n");
		return out;
	}

	for category in CATEGORIES {
		if !report.entries.iter().any(|entry| entry.category == *category) {
			continue;
		}
		out.push_str(&format!("## {category}\n\n"));
		for complexity in COMPLEXITIES {
			let group: Vec<&Entry> = report
				.entries
				.iter()
				.filter(|entry| entry.category == *category && entry.complexity == *complexity)
				.collect();
			if group.is_empty() {
				continue;
			}
			out.push_str(&format!("### {complexity}\n\n"));
			for entry in group {
				let issues = entry.issues.iter().map(|n| format!("#{n}")).collect::<Vec<_>>().join(", ");
				out.push_str(&format!(
					"- {} (#{}) by @{} — Wave issues: {}\n",
					entry.title, entry.number, entry.author, issues
				));
			}
			out.push('\n');
		}
	}

	if !report.manual.is_empty() {
		out.push_str("## Needs Manual Inclusion\n\n");
		out.push_str("These PRs link Wave issues that could not be fetched or parsed. Add them by hand.\n\n");
		for item in &report.manual {
			out.push_str(&format!(
				"- {} (#{}) by @{}: {}\n",
				item.title, item.number, item.author, item.reason
			));
		}
	}
	out
}
