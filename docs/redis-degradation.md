# Redis graceful degradation (#1335)

Shared signal (`cache::degradation`): `redis_degraded_operations_total{component,fallback}`, a rate-limited WARN with `degraded_mode=true dependency="redis" component fallback`, and the active component list on `/ready` (`X-Degraded-Dependencies: redis`).

| Path | Before | Degraded behaviour now |
|------|--------|------------------------|
| `services/query_cache.rs` | Startup hard-failed if Redis down; invalidation bypassed breaker | Lazy connect via breaker with connect/op timeouts; `get` → miss (direct DB read), `set` → memory-only, missed invalidations queued and replayed before next Redis read |
| `middleware/quota.rs` (rate limiting) | Local fallback at full limit; new breaker per request (never tripped) | Shared breaker per URL; 250 ms timeout; local limit at `RATE_LIMIT_DEGRADED_FRACTION` (default 0.5) — stricter, not fail-open; `X-RateLimit-Degraded` header |
| `middleware/idempotency.rs` | DB fallback only on connect failure; command errors ran request unprotected | Any Redis error → Postgres fallback (check and store) |
| `handlers/session.rs` | 500 | Fail closed with 503 + `Retry-After` |
| `services/webhook_dispatcher.rs` rate limit | Delivery attempt errored | Local per-endpoint limit at degraded fraction |
| Filter-rule cache invalidation, secrets pub/sub | Logged only | Skipped best-effort / poll-only, on shared signal |
| `auth/rate_limiting.rs`, `graphql/rate_limiting.rs`, `cache/rate_limiting.rs`, `cache/validation.rs`, `cache/webhook.rs` | In-process — no Redis dependency | Unchanged |
