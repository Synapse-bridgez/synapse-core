# Per-tenant latency histograms (#1337)

- Paths: webhook ingestion (`/webhook`, `/callback*`) and `/graphql`, timed outermost.
- Attribution: authenticated `TenantContext`, else `X-Tenant-ID` if it names a known tenant; 401/403 responses are never attributed.
- 12 fixed buckets, rolling 1 h window (12 × 5 min).
- Cardinality: top `TENANT_LATENCY_MAX_TRACKED_TENANTS` (50) tenants with ≥ `TENANT_LATENCY_MIN_REQUESTS` (50) requests get own series; rest → `_other`, unauthenticated → `_unattributed`; in-process cap `TENANT_LATENCY_MAX_TENANTS` (5000). Gauges `tenant_request_latency_window_{bucket,count,p95_ms}`.
- APIs: tenant-facing `GET /usage/latency` (own data, percentiles omitted below min sample), admin `GET /stats/tenant-latency`.
