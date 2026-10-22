# Swap Platform: Surveillance, Analytics, Market Data Archive

Covers #1505, #1506, #1508. Source feed: `swap_activity` (kind `quote`/`order`).
Background jobs are opt-in via env and are read-only with respect to order execution.

## Trade surveillance (#1505)

Enable: `SWAP_SURVEILLANCE_ENABLED=true`. Runs every `window/4` over the last aligned
window (`SWAP_SURVEILLANCE_WINDOW_SECS`, default 3600). Findings are idempotent per
`(rule, scope, window_start)` (`dedupe_key`), so reruns never duplicate rows or alerts.

| Rule | Severity | Fires when (defaults) |
| --- | --- | --- |
| `self_dealing` | high | >= 3 fills between different tenants sharing an `owner_group` on one pair |
| `wash_circular_flow` | medium | >= 4 fills for a tenant+pair and buy vs sell volume within 5% |
| `quote_probing` | low | >= 50 quotes for a tenant+pair and fills/quotes <= 0.02 |
| `volume_spike_favorable_pricing` | high | volume >= 5x the trailing 24-window average, >= 10,000, and volume-weighted price advantage >= 25 bps vs `ref_price` |

Thresholds: `SWAP_SURVEILLANCE_*` env vars (see `SurveillanceConfig::from_env`).
New rule = one `impl Rule` + entry in `default_rules`. High/critical new findings emit the
`SwapSurveillanceHighSeverity` alert. Automated suspension and ML scoring are out of scope.

**Operator API** (admin-auth, `/admin/swap/v1/surveillance/findings`): list with
`status`/`severity`/`tenant_id` filters; `POST .../:id/status` (`open|acknowledged|dismissed`);
`GET .../:id/audit`. Optional `X-Reviewer` header names the actor. Cross-tenant list queries
are themselves audited (`action=query`).

**Access and retention:** admin-only (no tenant access to findings). Findings are PII-adjacent;
`retention_until` defaults to 730 days and expired rows (with audit entries) are purged by the job.

Example finding:
```json
{"rule":"quote_probing","severity":"low","status":"open","tenant_ids":["..."],"pair":"USDC/XLM",
 "window_start":"2026-10-21T10:00:00Z","window_end":"2026-10-21T11:00:00Z",
 "evidence":{"quotes":60,"fills":0,"exec_ratio":0.0}}
```

## Analytics (#1506)

Enable the rollup worker with `SWAP_ANALYTICS_ENABLED=true` (every 60s).
`swap_analytics_buckets` holds hour/day/month buckets per tenant, pair, direction, status
(count, volume, avg slippage bps, median settlement ms). All buckets are **UTC**, half-open
`[start, end)`; month buckets follow calendar months.

- Rollups are recomputed per bucket (delete + insert in a transaction), so they are idempotent; each tick recomputes the current bucket plus 2 closed ones, absorbing late events. Use `recompute_bucket` to backfill/fix any window.
- **Freshness:** the response has `freshness.incomplete_from`; rows carry `complete`. Buckets containing "now" may change.
- **Endpoint:** `GET /swap/v1/analytics?granularity=hour|day|month&from&to[&pair&direction&status&limit&offset]`. Always scoped to the caller's tenant (`TenantContext`). Range caps: hour 31d, day 366d, month 5y; `limit` max 500.
- Index `(tenant_id, bucket_start, pair)` backs the query path.

## Market data archive (#1508)

Enable maintenance with `SWAP_ARCHIVE_ENABLED=true` (hourly). Monthly range partitions for
`swap_quote_archive` and `swap_price_snapshots` are created `SWAP_ARCHIVE_PARTITIONS_AHEAD`
(3) months ahead (`ensure_swap_archive_partitions`); partitions older than
`SWAP_ARCHIVE_RETENTION_MONTHS` (13, aligned with the swap retention policy) are detached
(`detach_swap_archive_partitions`) and queued in `swap_archive_exports`; they are dropped only
after an `ArchiveExporter` reports success. Export failure alerts (`SwapArchiveDegraded`) and retries.

- **Quotes:** full fidelity, written durably and synchronously (`record_quote`), contains tenant IDs (PII/retention policy applies).
- **Snapshots:** sampled (default every 5s per pair), write-behind via a bounded buffer (`SWAP_ARCHIVE_BUFFER_CAPACITY`); a full buffer drops the sample and a crash may lose the buffered tail. Failed flushes alert and drop rather than stall quoting.
- **Growth estimate:** a snapshot row is ~100 B (+ index ~60 B): 1 pair at 5s = 17,280 rows/day ~ 2.8 MB/day ~ 1 GB/yr per pair. A quote row is ~0.5-1 KB incl. JSON: 1M quotes/day ~ 0.5-1 GB/day, i.e. ~200-400 GB at 13 months retention. These are estimates from row shape, not measurements.
- **Read API:** `GET /admin/swap/v1/market-data/quotes` (`quote_id`, `order_id`, `tenant_id`, `pair`, `from`, `to`, `limit`) and `.../prices` (`pair`, `from`, `to`). Admin-only; no tenant access.

See also `docs/partitioning.md`.
