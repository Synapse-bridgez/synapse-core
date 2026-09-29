//! In-process alert payloads with automatic runbook links.
//!
//! Every alert this service raises itself goes through [`AlertPayload`],
//! which looks up the alert's `docs/runbook.md` section in
//! `alerting/runbook-links.json` (see [`runbook`]) and attaches it as
//! `runbook_url`, so an on-call operator goes straight from the page to the
//! remediation steps. Prometheus-evaluated alerts get the same link via the
//! `runbook_url` annotation in `alerting/prometheus-rules.yml`, which CI
//! checks against the same mapping.

pub mod names;
pub mod runbook;

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertSeverity {
    Info,
    Warning,
    Critical,
}

/// A single firing alert, ready to be logged / forwarded.
#[derive(Debug, Clone, Serialize)]
pub struct AlertPayload {
    pub alert: String,
    pub severity: AlertSeverity,
    pub summary: String,
    /// Link to the relevant runbook section; `None` only for alerts with a
    /// reviewed exemption in the mapping.
    pub runbook_url: Option<String>,
    pub labels: BTreeMap<String, String>,
    pub fired_at: DateTime<Utc>,
}

impl AlertPayload {
    pub fn new(alert: &str, severity: AlertSeverity, summary: impl Into<String>) -> Self {
        Self {
            alert: alert.to_string(),
            severity,
            summary: summary.into(),
            runbook_url: runbook::runbook_url(alert),
            labels: BTreeMap::new(),
            fired_at: Utc::now(),
        }
    }

    pub fn with_label(mut self, key: impl Into<String>, value: impl ToString) -> Self {
        self.labels.insert(key.into(), value.to_string());
        self
    }

    /// Logs the alert as a structured event. Alert routing (log-based
    /// alerting in the aggregator) keys on the `alert.name` field; the full
    /// payload, including `runbook_url`, is attached as `alert.payload`.
    pub fn emit(&self) {
        let payload = serde_json::to_string(self).unwrap_or_default();
        let runbook = self.runbook_url.as_deref().unwrap_or("");
        match self.severity {
            AlertSeverity::Critical => tracing::error!(
                alert.name = %self.alert,
                alert.severity = "critical",
                alert.runbook_url = %runbook,
                alert.payload = %payload,
                "{}",
                self.summary
            ),
            AlertSeverity::Warning => tracing::warn!(
                alert.name = %self.alert,
                alert.severity = "warning",
                alert.runbook_url = %runbook,
                alert.payload = %payload,
                "{}",
                self.summary
            ),
            AlertSeverity::Info => tracing::info!(
                alert.name = %self.alert,
                alert.severity = "info",
                alert.runbook_url = %runbook,
                alert.payload = %payload,
                "{}",
                self.summary
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_carries_runbook_url_and_labels() {
        let p = AlertPayload::new(
            names::PIPELINE_LATENCY_BUDGET_EXCEEDED,
            AlertSeverity::Warning,
            "processing over budget",
        )
        .with_label("stage", "processing");
        assert!(p
            .runbook_url
            .as_deref()
            .unwrap()
            .ends_with("#pipeline-latency-budget-exceeded"));
        assert_eq!(
            p.labels.get("stage").map(String::as_str),
            Some("processing")
        );

        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(json["severity"], "warning");
        assert!(json["runbook_url"].is_string());
    }

    #[test]
    fn exempt_alert_has_no_runbook_url() {
        let p = AlertPayload::new(names::QUOTA_CONFIG_SPLIT_BRAIN, AlertSeverity::Info, "x");
        assert!(p.runbook_url.is_none());
    }

    #[test]
    fn emit_all_severities() {
        for sev in [
            AlertSeverity::Info,
            AlertSeverity::Warning,
            AlertSeverity::Critical,
        ] {
            AlertPayload::new(names::SCHEDULED_JOB_FAILED, sev, "test").emit();
        }
    }
}
