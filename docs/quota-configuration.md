# Quota configuration architecture

## Source of truth

`tenants.rate_limit_per_minute` (Postgres) is the **single** source of
truth for the per-tenant rate limit. It is read into the in-memory
`AppState.tenant_configs` cache at startup and refreshed on a 60-second
background timer (`src/main.rs`), and consulted directly by
`middleware::quota::rate_limit_middleware` — the middleware that actually
enforces limits on every request.

`GET /admin/quotas`, `GET /admin/quotas/:tenant_id`, and
`PUT /admin/quotas/:tenant_id` (`src/handlers/admin/quota.rs`) all read and
write this same column now. There is no second config store in the write
path.

## Why there were two paths before this fix

`QuotaManager` (`src/middleware/quota.rs`) also has a Redis-backed
`quota:config:{key}` / `Tier`/`Quota` model with its own
`get_quota_config`/`set_quota_config`/`check_quota`/`consume_quota`
methods. That model is fully implemented and still exists (see
`tests/tenant_quota_test.rs`, which exercises it directly), but
`check_quota`/`consume_quota` — the only functions that ever read
`quota:config:*` — are never called from the live request path.
`rate_limit_middleware` computes its limit from `tenant_configs` instead.

Before this fix, `PUT /admin/quotas/:tenant_id` wrote to
`quota:config:{tenant_id}` via `QuotaManager::set_quota_config`. An admin
got a 200 and reasonably believed the enforced limit changed. It didn't —
live enforcement never read that key. `GET /admin/quotas/:tenant_id` still
showed correct-looking usage numbers throughout, because it reads the
*usage* counter (`quota:usage:*`), which the live middleware does write —
so a spot-check of "does the read reflect what I just wrote" partially
succeeded and hid the gap.

## What changed

- `PUT /admin/quotas/:tenant_id` now calls
  `queries::update_tenant_rate_limit`, writing directly to
  `tenants.rate_limit_per_minute` — the column `rate_limit_middleware`
  reads — and reloads `AppState.tenant_configs` inline so the change is
  enforced on the very next request rather than up to 60 seconds later.
- The request body's `tier` field is no longer accepted: it never mapped
  to anything the live enforcement path understood (that path has no
  concept of tiers at all), so silently accepting it without effect would
  be the same class of bug this fix closes. `custom_limit` — the per-minute
  number — is now required.
- A periodic background check
  (`middleware::quota::QuotaManager::scan_stale_quota_configs`, run every
  5 minutes from `main.rs`) scans for any remaining `quota:config:*` keys
  in Redis and logs a warning (`counter.quota_config_split_brain_detected_total`)
  if it finds any. Nothing should write to that key space anymore; any
  hits are either stale pre-fix data or a regression reintroducing the
  split-brain.

## Runbook: verifying a quota change actually took effect

Don't rely on `GET /admin/quotas/:tenant_id` alone to confirm a change —
it reads the same `tenants.rate_limit_per_minute` value the admin write
now updates, so before this fix it could look consistent with itself while
still disagreeing with live enforcement (see above). To actually confirm:

1. `PUT /admin/quotas/:tenant_id` with the new `custom_limit`.
2. Make real requests against a live-traffic endpoint (any route behind
   `rate_limit_middleware` — the callback/webhook routes, or any tenant
   data route) using that tenant's identity, and count how many succeed
   before a `429 Too Many Requests` with `X-RateLimit-Limit` in the
   response headers. `X-RateLimit-Limit` should match the value you just
   set.
3. If it doesn't match, check `counter.quota_config_updates_total` and
   `counter.quota_config_split_brain_detected_total` in logs/metrics — the
   former confirms the write happened, the latter flags exactly this kind
   of divergence.
