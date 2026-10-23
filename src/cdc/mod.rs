//! Change data capture (CDC) export of swap events to the analytics warehouse.
//!
//! Capture uses native PostgreSQL logical decoding (publication + slot, see
//! `migrations/20261021000001_cdc_publication.sql`) read by an external
//! connector (`deploy/cdc/connector.json`). This module holds the connector
//! settings, lag/slot health evaluation used by `/health` and alerting, the
//! PII exclusion list, and the idempotent staging key. See `docs/cdc.md`.

use serde::{Deserialize, Serialize};

pub const PUBLICATION: &str = "synapse_swap_cdc";
pub const SLOT: &str = "synapse_swap_cdc_slot";

/// Columns never exported to the warehouse (PII / secrets).
pub const PII_EXCLUDED_COLUMNS: &[&str] = &["stellar_account", "email", "metadata", "api_key"];

#[derive(Debug, Clone)]
pub struct CdcConfig {
    /// Alert when the connector is more than this many seconds behind.
    pub max_lag_secs: u64,
    /// Alert when retained WAL for the slot exceeds this many bytes.
    pub max_slot_retained_bytes: u64,
}

impl Default for CdcConfig {
    fn default() -> Self {
        Self {
            max_lag_secs: 300,
            max_slot_retained_bytes: 5 * 1024 * 1024 * 1024,
        }
    }
}

impl CdcConfig {
    pub fn from_env() -> Self {
        let d = Self::default();
        let get = |k: &str, dv: u64| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(dv)
        };
        Self {
            max_lag_secs: get("CDC_MAX_LAG_SECS", d.max_lag_secs),
            max_slot_retained_bytes: get("CDC_MAX_SLOT_RETAINED_BYTES", d.max_slot_retained_bytes),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CdcStatus {
    Healthy,
    LagHigh,
    SlotBloat,
    SlotMissing,
}

/// Observed state of the replication slot / connector.
#[derive(Debug, Clone)]
pub struct CdcObservation {
    pub slot_exists: bool,
    pub lag_secs: u64,
    pub slot_retained_bytes: u64,
}

/// Evaluate connector health; WAL bloat takes precedence over lag.
pub fn evaluate(cfg: &CdcConfig, obs: &CdcObservation) -> CdcStatus {
    if !obs.slot_exists {
        CdcStatus::SlotMissing
    } else if obs.slot_retained_bytes > cfg.max_slot_retained_bytes {
        CdcStatus::SlotBloat
    } else if obs.lag_secs > cfg.max_lag_secs {
        CdcStatus::LagHigh
    } else {
        CdcStatus::Healthy
    }
}

/// Idempotency key for staging rows: `(aggregate id, version)`. Replays
/// upsert on this key, so they never double-count; delivery is at-least-once
/// and ordered per aggregate by version.
pub fn staging_key(aggregate_id: &str, version: u64) -> String {
    format!("{aggregate_id}:{version}")
}

/// Drop PII columns from a captured row before it reaches the warehouse.
pub fn strip_pii(row: &mut serde_json::Map<String, serde_json::Value>) {
    for c in PII_EXCLUDED_COLUMNS {
        row.remove(*c);
    }
}

/// Apply a staging upsert: keeps exactly one row per `(aggregate, version)`.
pub fn dedupe_events(events: Vec<(String, u64)>) -> Vec<(String, u64)> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out: Vec<_> = events
        .into_iter()
        .filter(|(a, v)| seen.insert(staging_key(a, *v)))
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lag_and_bloat_alerts() {
        let cfg = CdcConfig::default();
        let ok = CdcObservation {
            slot_exists: true,
            lag_secs: 1,
            slot_retained_bytes: 1,
        };
        assert_eq!(evaluate(&cfg, &ok), CdcStatus::Healthy);
        assert_eq!(
            evaluate(
                &cfg,
                &CdcObservation {
                    lag_secs: 9999,
                    ..ok.clone()
                }
            ),
            CdcStatus::LagHigh
        );
        assert_eq!(
            evaluate(
                &cfg,
                &CdcObservation {
                    slot_retained_bytes: u64::MAX,
                    ..ok.clone()
                }
            ),
            CdcStatus::SlotBloat
        );
        assert_eq!(
            evaluate(
                &cfg,
                &CdcObservation {
                    slot_exists: false,
                    ..ok
                }
            ),
            CdcStatus::SlotMissing
        );
    }

    #[test]
    fn replay_lifecycle_appears_exactly_once() {
        let lifecycle = vec![
            ("o1".to_string(), 1),
            ("o1".to_string(), 2),
            ("o1".to_string(), 3),
        ];
        let replayed: Vec<_> = lifecycle
            .iter()
            .cloned()
            .chain(lifecycle.iter().cloned())
            .collect();
        assert_eq!(dedupe_events(replayed), lifecycle);
    }

    #[test]
    fn pii_is_excluded() {
        let mut row = serde_json::json!({"id": "1", "stellar_account": "G..", "amount": "5"})
            .as_object()
            .unwrap()
            .clone();
        strip_pii(&mut row);
        assert!(!row.contains_key("stellar_account") && row.contains_key("amount"));
    }
}
