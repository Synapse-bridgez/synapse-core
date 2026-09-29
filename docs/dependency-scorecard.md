# Dependency health scorecard (#1334)

Tracks uptime, attributed error rate and p50/p95/p99 latency for `settlement_api` (Horizon), `redis`, `postgres` and `vault` over rolling 7/30/90-day windows.

- **Sources (no bespoke instrumentation):** `RedisCircuitBreaker::call`, `db::queries::with_timeout`, `HorizonClient` circuit-breaker call sites, Vault refresh reads, plus circuit-breaker transitions (incl. `services::circuit_breaker`).
- **Outage vs. our partition:** a response with an error is always the dependency's fault. Transport failures (no response) are classified per minute: if ≥2 dependencies had traffic and *all* were failing at the transport level, the minute is a `local_partition` — reported as `partition_failures`, excluded from uptime.
- **Storage:** per-minute in-process buckets → 5-minute rows in `dependency_health_rollups` (per instance, mergeable latency buckets), 100-day retention.
- **Query:** `GET /admin/dependencies/scorecard?windows=7,30,90` (JSON), `/raw?dependency=&since=&until=&format=json|csv` (capacity planning / postmortems), `/dashboard` (HTML). Gauges: `dependency_scorecard_{uptime_ratio,error_rate,partition_error_rate,latency_p95_ms}{dependency,window}`.
- **Grades:** healthy (≥99.9% uptime, ≤1% errors), watch (≥99%, ≤5%), needs_investment, insufficient_data (<100 calls or <50% coverage). Reporting only — no automated remediation.
