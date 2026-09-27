# Metrics Label Cardinality Convention

Every metric label (an `opentelemetry::KeyValue` passed to a counter,
histogram, or gauge) creates one time series per distinct label value seen.
An unbounded label — a raw ID, a raw count, a URL, a free-text string —
grows the metrics backend's series count without bound and is a common
source of cost and performance incidents at scale.

## Allowed label dimensions

Only label values drawn from a small, closed set are safe:

- Enum-like strings with a fixed set of variants: `operation`, `outcome`,
  `result`, `stage`, `transition`, `reason`.
- A bounded, application-defined identifier such as `asset_code` (the set
  of supported assets is small and centrally configured, not user-supplied
  free text).
- A UUID that identifies a small, admin-configured resource (e.g.
  `endpoint_id` for webhook endpoints) is acceptable *only* when the number
  of such resources is operationally bounded (tens–hundreds, not
  per-request or per-transaction). Document the reasoning inline if you add
  one of these.

## Disallowed label dimensions

- Raw tenant ID, user ID, transaction ID, or any other per-entity
  identifier that grows with data volume.
- Raw numeric counts or measurements (e.g. a per-call transaction count) —
  these belong in the metric's *value*, not its label set. Use a `Counter`
  incremented by the count, or record the count as the histogram's
  observation, never as a `KeyValue`.
- Raw URLs, file paths, or other free-text/unbounded strings.

## Reviewed exceptions

Any exception must be documented at the call site with a comment
explaining why the label's cardinality is actually bounded.

| File | Label | Why it is bounded |
|------|-------|-------------------|
| `src/tenant/latency.rs` | `tenant_id` | Per-tenant latency gauges (#1337) export only the top `TENANT_LATENCY_MAX_TRACKED_TENANTS` tenants (default 50, hard cap 200) that also meet `TENANT_LATENCY_MIN_REQUESTS` in the rolling window; every other tenant is folded into `tenant_id="_other"`, unauthenticated traffic into `"_unattributed"`. Series are rolling-window gauges computed at collection time, so demoted tenants disappear instead of accumulating. Bound: `(K + 2) × 2 routes × 12 buckets`. Enforced by `tenant::latency::tests::cardinality_is_bounded_under_large_synthetic_load`. See docs/tenant-latency-histograms.md. |

## Enforcement

`tests/metrics_cardinality_test.rs` statically scans `src/` for
`KeyValue::new("<label>"` call sites using a denylist of known-unbounded
label names (`user_id`, `tenant_id`, `transaction_id`, `account_id`,
`request_id`, `session_id`, `url`, `path`, `count`, `transaction_count`).
Adding a new metric with one of these label names fails that test; either
rename/bucket the label or add a reviewed exception to this document and
the test's allowlist.
