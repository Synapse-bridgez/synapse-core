//! Alert-name → `docs/runbook.md` section mapping.
//!
//! The mapping lives in `alerting/runbook-links.json`, which is the single
//! source of truth read by both this module (embedded at compile time, so a
//! running binary can never disagree with the mapping it was built from)
//! and the CI validator `scripts/check-alert-runbook-links.py`.
//!
//! Each entry is either `{"anchor": "<runbook heading slug>"}` or an explicit
//! `{"exempt": {"reason": ..., "reviewed_by": ..., "reviewed_on": ...}}` —
//! an alert without a runbook link has to say so on purpose.

use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;

/// The mapping file, embedded at build time.
pub const MAPPING_JSON: &str = include_str!("../../alerting/runbook-links.json");

/// The runbook the mapping points into, embedded at build time so the
/// anchor check in the tests below runs against the exact same revision.
pub const RUNBOOK_MARKDOWN: &str = include_str!("../../docs/runbook.md");

/// Overrides the mapping's `base_url` (e.g. for an internally hosted copy
/// of the docs).
pub const BASE_URL_ENV: &str = "RUNBOOK_BASE_URL";

#[derive(Debug, Clone, Deserialize)]
pub struct Exemption {
    pub reason: String,
    pub reviewed_by: String,
    pub reviewed_on: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MappingEntry {
    #[serde(default)]
    pub anchor: Option<String>,
    #[serde(default)]
    pub exempt: Option<Exemption>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunbookMapping {
    pub runbook: String,
    pub base_url: String,
    pub alerts: BTreeMap<String, MappingEntry>,
}

/// A problem found while validating the mapping against the runbook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappingProblem {
    /// Entry has both or neither of `anchor`/`exempt`.
    AmbiguousEntry { alert: String },
    /// `anchor` does not match any heading in the runbook.
    DeadAnchor { alert: String, anchor: String },
    /// Exemption is missing its reason or reviewer.
    UnreviewedExemption { alert: String },
    /// An alert the service raises has no mapping entry at all.
    Unmapped { alert: String },
}

impl std::fmt::Display for MappingProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AmbiguousEntry { alert } => {
                write!(
                    f,
                    "{alert}: entry must have exactly one of `anchor` or `exempt`"
                )
            }
            Self::DeadAnchor { alert, anchor } => {
                write!(f, "{alert}: anchor #{anchor} does not exist in the runbook")
            }
            Self::UnreviewedExemption { alert } => {
                write!(
                    f,
                    "{alert}: exemption needs a non-empty reason and reviewed_by"
                )
            }
            Self::Unmapped { alert } => {
                write!(f, "{alert}: no runbook link or exemption in the mapping")
            }
        }
    }
}

impl RunbookMapping {
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Runbook URL for `alert`, or `None` if it is exempt or unmapped.
    pub fn url_for(&self, alert: &str, base_url: &str) -> Option<String> {
        let anchor = self.alerts.get(alert)?.anchor.as_deref()?;
        Some(format!("{}#{}", base_url.trim_end_matches('#'), anchor))
    }

    /// Validates every entry against the headings in `runbook_markdown`, and
    /// that every name in `required_alerts` has an entry.
    pub fn validate(
        &self,
        runbook_markdown: &str,
        required_alerts: &[&str],
    ) -> Vec<MappingProblem> {
        let anchors = heading_anchors(runbook_markdown);
        let mut problems = Vec::new();

        for (alert, entry) in &self.alerts {
            match (&entry.anchor, &entry.exempt) {
                (Some(anchor), None) => {
                    if !anchors.contains(anchor) {
                        problems.push(MappingProblem::DeadAnchor {
                            alert: alert.clone(),
                            anchor: anchor.clone(),
                        });
                    }
                }
                (None, Some(exemption)) => {
                    if exemption.reason.trim().is_empty() || exemption.reviewed_by.trim().is_empty()
                    {
                        problems.push(MappingProblem::UnreviewedExemption {
                            alert: alert.clone(),
                        });
                    }
                }
                _ => problems.push(MappingProblem::AmbiguousEntry {
                    alert: alert.clone(),
                }),
            }
        }

        for alert in required_alerts {
            if !self.alerts.contains_key(*alert) {
                problems.push(MappingProblem::Unmapped {
                    alert: alert.to_string(),
                });
            }
        }

        problems
    }
}

/// The embedded mapping, parsed once. Panics only if the embedded JSON is
/// malformed, which the unit tests below rule out at build/test time.
pub fn mapping() -> &'static RunbookMapping {
    static MAPPING: OnceLock<RunbookMapping> = OnceLock::new();
    MAPPING.get_or_init(|| {
        RunbookMapping::parse(MAPPING_JSON).expect("alerting/runbook-links.json is malformed")
    })
}

/// Runbook URL for an in-process alert, honouring [`BASE_URL_ENV`].
pub fn runbook_url(alert: &str) -> Option<String> {
    let mapping = mapping();
    let base = std::env::var(BASE_URL_ENV).unwrap_or_else(|_| mapping.base_url.clone());
    mapping.url_for(alert, &base)
}

/// GitHub-style anchor slug for a Markdown heading: lowercase, drop
/// everything that isn't alphanumeric / `-` / `_` / space, then turn spaces
/// into hyphens. Mirrors `slugify` in `scripts/check-alert-runbook-links.py`.
pub fn slugify(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            '-' | '_' => Some(c),
            c if c.is_alphanumeric() => Some(c),
            _ => None,
        })
        .collect()
}

/// Every heading anchor in a Markdown document, skipping `#` lines inside
/// fenced code blocks and disambiguating repeated headings with `-1`, `-2`,
/// … the way GitHub does.
pub fn heading_anchors(markdown: &str) -> HashSet<String> {
    let mut anchors = HashSet::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut in_fence = false;

    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if hashes == 0 || hashes > 6 || !trimmed[hashes..].starts_with(' ') {
            continue;
        }
        let text = trimmed[hashes..].trim().trim_end_matches('#').trim();
        let base = slugify(text);
        let count = seen.entry(base.clone()).or_insert(0);
        let slug = if *count == 0 {
            base.clone()
        } else {
            format!("{base}-{count}")
        };
        *count += 1;
        anchors.insert(slug);
    }

    anchors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_matches_github_rules() {
        assert_eq!(slugify("Monitoring & Alerting"), "monitoring--alerting");
        assert_eq!(slugify("3. High Error Rate"), "3-high-error-rate");
        assert_eq!(
            slugify("Symptom: Pool usage consistently high (>80%)"),
            "symptom-pool-usage-consistently-high-80"
        );
        assert_eq!(
            slugify("6. Missing-Partition (23514) Burst"),
            "6-missing-partition-23514-burst"
        );
        assert_eq!(slugify("snake_case `code`"), "snake_case-code");
    }

    #[test]
    fn heading_anchors_skip_code_fences_and_dedupe() {
        let md =
            "# Top\n## Tasks\n```bash\n# Not a heading\n```\n### Tasks\n####NoSpace\n## Tasks ##\n";
        let anchors = heading_anchors(md);
        assert!(anchors.contains("top"));
        assert!(anchors.contains("tasks"));
        assert!(anchors.contains("tasks-1"));
        assert!(anchors.contains("tasks-2"));
        assert!(!anchors.contains("not-a-heading"));
        assert!(!anchors.contains("nospace"));
    }

    #[test]
    fn embedded_mapping_parses() {
        let m = mapping();
        assert_eq!(m.runbook, "docs/runbook.md");
        assert!(!m.alerts.is_empty());
    }

    /// The CI guarantee: every mapped anchor resolves to a real heading in
    /// the runbook revision this binary was built with, every exemption is
    /// reviewed, and every in-process alert is covered.
    #[test]
    fn every_mapping_entry_resolves_to_a_runbook_anchor() {
        let problems = mapping().validate(RUNBOOK_MARKDOWN, crate::alerting::names::ALL);
        assert!(
            problems.is_empty(),
            "runbook mapping problems:\n{}",
            problems
                .iter()
                .map(|p| format!("  - {p}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn renamed_heading_is_reported_as_dead_anchor() {
        let mapping = RunbookMapping::parse(
            r#"{"runbook":"r.md","base_url":"https://x/r.md","alerts":{
                "A":{"anchor":"old-name"},
                "B":{"anchor":"kept"}}}"#,
        )
        .unwrap();
        let problems = mapping.validate("## New Name\n## Kept\n", &[]);
        assert_eq!(
            problems,
            vec![MappingProblem::DeadAnchor {
                alert: "A".into(),
                anchor: "old-name".into()
            }]
        );
    }

    #[test]
    fn invalid_entries_are_reported() {
        let mapping = RunbookMapping::parse(
            r#"{"runbook":"r.md","base_url":"https://x/r.md","alerts":{
                "Both":{"anchor":"a","exempt":{"reason":"r","reviewed_by":"me","reviewed_on":"2026-01-01"}},
                "Neither":{},
                "Lazy":{"exempt":{"reason":" ","reviewed_by":"me","reviewed_on":"2026-01-01"}}}}"#,
        )
        .unwrap();
        let problems = mapping.validate("## A\n", &["Missing"]);
        assert!(problems.contains(&MappingProblem::AmbiguousEntry {
            alert: "Both".into()
        }));
        assert!(problems.contains(&MappingProblem::AmbiguousEntry {
            alert: "Neither".into()
        }));
        assert!(problems.contains(&MappingProblem::UnreviewedExemption {
            alert: "Lazy".into()
        }));
        assert!(problems.contains(&MappingProblem::Unmapped {
            alert: "Missing".into()
        }));
        for p in &problems {
            assert!(!p.to_string().is_empty());
        }
    }

    #[test]
    fn url_for_handles_exempt_and_unknown() {
        let mapping = RunbookMapping::parse(
            r##"{"runbook":"r.md","base_url":"https://x/r.md#","alerts":{
                "A":{"anchor":"a"},
                "E":{"exempt":{"reason":"r","reviewed_by":"me","reviewed_on":"2026-01-01"}}}}"##,
        )
        .unwrap();
        assert_eq!(
            mapping.url_for("A", &mapping.base_url).as_deref(),
            Some("https://x/r.md#a")
        );
        assert_eq!(mapping.url_for("E", &mapping.base_url), None);
        assert_eq!(mapping.url_for("Nope", &mapping.base_url), None);
    }

    #[test]
    fn runbook_url_resolves_for_mapped_in_process_alert() {
        let url = runbook_url(crate::alerting::names::TOKIO_TASK_LEAK_SUSPECTED).unwrap();
        assert!(url.contains("docs/runbook.md#"), "{url}");
    }
}
