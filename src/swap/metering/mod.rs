//! Usage metering and billable event emission for swap activity.
//!
//! This module defines a versioned usage-event schema and the lifecycle points
//! at which billable events are emitted. Events are intended to be written in
//! the same transaction as the lifecycle state change and published via the
//! outbox pipeline so that at-least-once delivery plus a stable idempotency key
//! yields exactly-once effect downstream.
//!
//! Rollups are derived exclusively from raw events so they can always be
//! rebuilt. Corrections (refunds, compensation) are represented as negative
//! adjustment events rather than mutating history.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Current version of the usage-event schema. Bump when the wire format
/// changes in a backwards-incompatible way.
pub const USAGE_EVENT_SCHEMA_VERSION: u16 = 1;

/// The metric being metered for a tenant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricType {
    /// A quote was served to a client.
    QuoteServed,
    /// An order was accepted into the engine.
    OrderAccepted,
    /// An order completed successfully.
    OrderCompleted,
    /// Notional value that was filled.
    NotionalFilled,
    /// A negative adjustment (refund/compensation) applied to a prior metric.
    Adjustment,
}

impl MetricType {
    /// Stable string identifier used in persistence and rollups.
    pub fn as_str(&self) -> &'static str {
        match self {
            MetricType::QuoteServed => "quote_served",
            MetricType::OrderAccepted => "order_accepted",
            MetricType::OrderCompleted => "order_completed",
            MetricType::NotionalFilled => "notional_filled",
            MetricType::Adjustment => "adjustment",
        }
    }
}

impl fmt::Display for MetricType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A lifecycle point at which a billable event is emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecyclePoint {
    QuoteServed,
    OrderAccepted,
    OrderCompleted,
    NotionalFilled,
}

impl LifecyclePoint {
    /// The metric emitted at this lifecycle point.
    pub fn metric(&self) -> MetricType {
        match self {
            LifecyclePoint::QuoteServed => MetricType::QuoteServed,
            LifecyclePoint::OrderAccepted => MetricType::OrderAccepted,
            LifecyclePoint::OrderCompleted => MetricType::OrderCompleted,
            LifecyclePoint::NotionalFilled => MetricType::NotionalFilled,
        }
    }
}

/// A durable, deduplicated billable usage event.
///
/// Contains no counterparty detail (PII minimization): only the tenant, the
/// period, the metric, the quantity, and a stable idempotency key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageEvent {
    /// Schema version of this event.
    pub schema_version: u16,
    /// Tenant that consumed the usage.
    pub tenant_id: String,
    /// Billing period (e.g. `2024-06`) the event is attributed to.
    pub period: String,
    /// Metric being metered.
    pub metric: MetricType,
    /// Quantity consumed. Negative for adjustment events.
    pub quantity: i64,
    /// Stable idempotency key; retries and replays reuse the same key.
    pub idempotency_key: String,
    /// Optional reference to the originating order/quote (no PII).
    pub source_ref: Option<String>,
}

impl UsageEvent {
    /// Construct a new usage event at the current schema version.
    pub fn new(
        tenant_id: impl Into<String>,
        period: impl Into<String>,
        metric: MetricType,
        quantity: i64,
        idempotency_key: impl Into<String>,
        source_ref: Option<String>,
    ) -> Self {
        UsageEvent {
            schema_version: USAGE_EVENT_SCHEMA_VERSION,
            tenant_id: tenant_id.into(),
            period: period.into(),
            metric,
            quantity,
            idempotency_key: idempotency_key.into(),
            source_ref,
        }
    }

    /// Build the idempotency key for a lifecycle event.
    ///
    /// The key is derived from the tenant, the lifecycle point, and the source
    /// reference so that retries of the same lifecycle transition collapse to a
    /// single billable event.
    pub fn idempotency_key_for(
        tenant_id: &str,
        point: LifecyclePoint,
        source_ref: &str,
    ) -> String {
        format!("{}:{}:{}", tenant_id, point.metric().as_str(), source_ref)
    }

    /// Emit a usage event for a lifecycle point.
    pub fn for_lifecycle(
        tenant_id: impl Into<String>,
        period: impl Into<String>,
        point: LifecyclePoint,
        quantity: i64,
        source_ref: impl Into<String>,
    ) -> Self {
        let tenant_id = tenant_id.into();
        let source_ref = source_ref.into();
        let key = Self::idempotency_key_for(&tenant_id, point, &source_ref);
        UsageEvent::new(
            tenant_id,
            period,
            point.metric(),
            quantity,
            key,
            Some(source_ref),
        )
    }

    /// Emit a negative adjustment event (refund/compensation) against a prior
    /// metric. The adjustment carries its own idempotency key so it is applied
    /// at most once.
    pub fn adjustment(
        tenant_id: impl Into<String>,
        period: impl Into<String>,
        quantity: i64,
        adjustment_ref: impl Into<String>,
    ) -> Self {
        let tenant_id = tenant_id.into();
        let adjustment_ref = adjustment_ref.into();
        let key = format!("{}:adjustment:{}", tenant_id, adjustment_ref);
        UsageEvent::new(
            tenant_id,
            period,
            MetricType::Adjustment,
            -quantity.abs(),
            key,
            Some(adjustment_ref),
        )
    }
}

/// Per-tenant, per-period aggregate derived exclusively from raw events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRollup {
    pub tenant_id: String,
    pub period: String,
    pub metric: MetricType,
    /// Sum of quantities for the metric in the period.
    pub total_quantity: i64,
    /// Number of distinct events contributing to the aggregate.
    pub event_count: i64,
}

/// Aggregate a set of raw events into per-tenant, per-period rollups.
///
/// Deduplicates by idempotency key so retries and replays never double-count.
/// Rollups are always derived from raw events, so they can be rebuilt at any
/// time by replaying the event log through this function.
pub fn rollup(events: &[UsageEvent]) -> Vec<UsageRollup> {
    use std::collections::{BTreeMap, HashSet};

    let mut seen: HashSet<&str> = HashSet::new();
    let mut aggregates: BTreeMap<(String, String, MetricType), (i64, i64)> = BTreeMap::new();

    for event in events {
        if !seen.insert(event.idempotency_key.as_str()) {
            continue;
        }
        let key = (event.tenant_id.clone(), event.period.clone(), event.metric);
        let entry = aggregates.entry(key).or_insert((0, 0));
        entry.0 += event.quantity;
        entry.1 += 1;
    }

    aggregates
        .into_iter()
        .map(|((tenant_id, period, metric), (total_quantity, event_count))| UsageRollup {
            tenant_id,
            period,
            metric,
            total_quantity,
            event_count,
        })
        .collect()
}

/// Reconcile raw events against previously computed rollups.
///
/// Returns the rollups recomputed from raw events so callers can compare them
/// against persisted rollups. Any mismatch indicates a rollup that must be
/// rebuilt; corrections are applied as adjustment events, never by mutating
/// raw history.
pub fn reconcile(events: &[UsageEvent]) -> Vec<UsageRollup> {
    rollup(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(tenant: &str, period: &str, metric: MetricType, qty: i64, key: &str) -> UsageEvent {
        UsageEvent::new(tenant, period, metric, qty, key, None)
    }

    #[test]
    fn deduplicates_by_idempotency_key() {
        let events = vec![
            event("t1", "2024-06", MetricType::OrderCompleted, 1, "k1"),
            event("t1", "2024-06", MetricType::OrderCompleted, 1, "k1"),
            event("t1", "2024-06", MetricType::OrderCompleted, 1, "k2"),
        ];
        let rollups = rollup(&events);
        assert_eq!(rollups.len(), 1);
        assert_eq!(rollups[0].total_quantity, 2);
        assert_eq!(rollups[0].event_count, 2);
    }

    #[test]
    fn aggregates_per_tenant_and_period() {
        let events = vec![
            event("t1", "2024-06", MetricType::NotionalFilled, 100, "a"),
            event("t1", "2024-07", MetricType::NotionalFilled, 50, "b"),
            event("t2", "2024-06", MetricType::NotionalFilled, 25, "c"),
        ];
        let rollups = rollup(&events);
        assert_eq!(rollups.len(), 3);
    }

    #[test]
    fn adjustment_is_negative_and_deduplicated() {
        let adj = UsageEvent::adjustment("t1", "2024-06", 10, "refund-1");
        assert_eq!(adj.quantity, -10);
        assert_eq!(adj.metric, MetricType::Adjustment);
        let rollups = rollup(&[adj.clone(), adj]);
        assert_eq!(rollups.len(), 1);
        assert_eq!(rollups[0].total_quantity, -10);
    }

    #[test]
    fn lifecycle_idempotency_key_is_stable() {
        let a = UsageEvent::for_lifecycle("t1", "2024-06", LifecyclePoint::OrderCompleted, 1, "order-9");
        let b = UsageEvent::for_lifecycle("t1", "2024-06", LifecyclePoint::OrderCompleted, 1, "order-9");
        assert_eq!(a.idempotency_key, b.idempotency_key);
        assert_eq!(rollup(&[a, b])[0].total_quantity, 1);
    }

    #[test]
    fn rebuild_matches_raw_events() {
        let events = vec![
            event("t1", "2024-06", MetricType::QuoteServed, 1, "q1"),
            event("t1", "2024-06", MetricType::OrderAccepted, 1, "o1"),
            event("t1", "2024-06", MetricType::OrderCompleted, 1, "o1c"),
        ];
        assert_eq!(rollup(&events), reconcile(&events));
    }
}
