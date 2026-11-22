# Auth rate-limiting architecture

## What exists

`AuthRateLimiter` (`src/auth/rate_limiting.rs`) is a per-identity
token-bucket rate limiter for authentication operations. It does not
implement its own bucket arithmetic: every bucket is a
`cache::rate_limiting::RateLimiter` (`src/cache/rate_limiting.rs`), so the
auth layer and the cache layer share one token-bucket implementation.

It is used by the two live authentication middlewares —
`middleware::auth::admin_auth` (guards every `/admin/*` route) and
`middleware::auth::api_key_auth` — and by the tenant API-key resolution in
`TenantContext` (`src/tenant/mod.rs`) and `/ws` token authentication
(`handlers::ws::authenticate_ws_token`), both of which check credentials
the same way (`tenants.api_key` lookup).

## Algorithm

Token bucket, one bucket per identity, refilled lazily on access
(`RateLimiter::refill_tokens` in `src/cache/rate_limiting.rs`):

- A bucket starts full at `max_requests` tokens.
- `try_acquire()` first refills, then decrements one token via a
  compare-and-swap loop on an `AtomicU32`; it fails only when the bucket
  holds fewer tokens than requested.
- Refill is time-based and integer-only (no floating point, so no drift):
  - if a full `window` has elapsed since the last refill, the bucket is
    reset to `max_requests`;
  - otherwise `tokens_to_add = max_requests * elapsed_ms / window_ms`, and
    the bucket is capped at `max_requests`.
- `time_until_available()` returns `Duration::ZERO` when a token is
  available, otherwise the remainder of the current window. The auth layer
  turns that into `AuthError::RateLimited(retry_after_secs)`.

`AuthRateLimiter` adds three things on top of the shared bucket:

- **Key validation before allocation.** `validate_identity_key` rejects
  empty keys, keys longer than 256 bytes, and keys containing anything
  outside ASCII alphanumerics plus `-`, `_`, `.`, `:` (the colon allows
  `ip:1.2.3.4`-style keys). Malformed keys never create a bucket entry.
- **Two limit classes.** Per-identity auth attempts
  (`check_auth_rate_limit`) and a single shared Vault health-probe bucket
  (`check_vault_probe_rate_limit`).
- **Metrics.** Attempts, successes and failures are recorded through
  `AuthMetrics` (`src/auth/metrics.rs`).

## Storage

- **Per-identity auth buckets:** `Arc<Mutex<HashMap<String, RateLimiter>>>`
  inside `AuthRateLimiter`. The mutex is held only for the map
  lookup/insert (`get_or_create_auth_bucket`), never while acquiring a
  token, so contention stays low. Cloning an `AuthRateLimiter` is O(1) and
  shares the same map and metrics.
- **Vault probe bucket:** a single `RateLimiter` field, shared by all
  callers.
- **Process-wide instances:** two `lazy_static!` globals in
  `src/auth/rate_limiting.rs` — `ADMIN_AUTH_RATE_LIMITER` and
  `TENANT_AUTH_RATE_LIMITER` — each `AuthRateLimiter::new()` with default
  configuration. They are separate so a flood of guessed admin keys and a
  flood of guessed tenant API keys do not share (and drain) the same
  budget for a given source IP.
- **No external store.** Buckets live in process memory only; nothing is
  written to Redis or Postgres, and each process has its own copy (see
  *Break-glass* below).

## Configuration

The limiter reads no environment variables of its own. Limits come from
`AuthRateLimitConfig` (`src/auth/rate_limiting.rs`), whose `Default` impl
is what both globals use:

| Field | Default | Meaning |
|-------|---------|---------|
| `auth_limit` | `10` (`DEFAULT_AUTH_LIMIT`) | Auth attempts per identity per window |
| `vault_probe_limit` | `5` (`DEFAULT_VAULT_PROBE_LIMIT`) | Vault health probes per window |
| `window` | `60s` (`DEFAULT_AUTH_WINDOW`) | Refill window for both limit classes |

`src/config.rs` does **not** define any auth rate-limit keys. The
`DEFAULT_RATE_LIMIT` / `WHITELIST_RATE_LIMIT` values set there are
profile defaults for the general request limiter, not for
`AuthRateLimiter`, and are not read by it. The only environment variable
the auth path reads is `ADMIN_AUTH_RATE_LIMIT_MODE`, and it is read in
`middleware::auth::admin_auth` (shadow mode), not by the limiter itself.

## What changed

Two process-wide, shared instances (`src/auth/rate_limiting.rs`, following
the same `lazy_static!` global-state pattern this codebase already uses
for `SESSION_STORE` — chosen over adding fields to `AppState` because that
struct is constructed as a literal in ~10 test files, and the two are
logically independent of per-request app state anyway):

- `ADMIN_AUTH_RATE_LIMITER` — guards `admin_auth`.
- `TENANT_AUTH_RATE_LIMITER` — guards `api_key_auth`, `TenantContext`'s
  API-key resolution, and `/ws` token authentication. Kept separate from
  the admin limiter so a flood of guessed admin keys and a flood of
  guessed tenant API keys don't share (and drain) the same bucket budget
  for a given source IP.

Both are keyed by source IP (`ip:{addr}`, extracted from the
`ConnectInfo<SocketAddr>` request extension) and use the limiter's
defaults (10 attempts / 60s window per identity — see
`AuthRateLimitConfig`). Exceeding the limit returns
`429 Too Many Requests` and emits a per-middleware counter:
`counter.admin_auth_lockout_triggered_total`,
`counter.api_key_auth_lockout_triggered_total`,
`counter.tenant_auth_lockout_triggered_total`,
`counter.ws_auth_lockout_triggered_total`.

## Rollout: admin path shadow mode

A false lockout of a legitimate admin during an incident is its own
operational risk — the issue this fixes calls this out explicitly. Rather
than ship a second PR for a staged rollout, `admin_auth` supports a
shadow/log-only mode via the `ADMIN_AUTH_RATE_LIMIT_MODE` environment
variable:

- Unset (default): enforcing. Exceeding the limit returns 429.
- `ADMIN_AUTH_RATE_LIMIT_MODE=shadow`: logs what would have been rejected
  (`counter.admin_auth_lockout_triggered_total` with `shadow_mode=true`)
  but still runs the request through. Use this for one deploy cycle to
  observe real-world trigger rates against actual admin traffic patterns
  before removing the variable (or setting it to any other value) to
  enforce.

`api_key_auth`/`TenantContext`/`/ws` do not have a shadow mode — they gate
tenant-facing traffic, not a single shared admin credential, so the
"false lockout of the one admin during an incident" risk doesn't apply the
same way, and this is the same PR that's establishing tenant credential
checking as live for the first time (see
`docs/postmortem-cross-tenant-leak.md`).

## Break-glass: legitimately locked-out admin

`AuthRateLimiter`'s buckets are in-process memory (`Arc<Mutex<HashMap>>`),
not persisted anywhere. If a legitimate admin is locked out:

- **Restarting the process clears every bucket.** This is the immediate,
  always-available unlock path — no code change or special procedure
  needed. In a multi-instance deployment, the lockout is per-instance
  (each process has its own `ADMIN_AUTH_RATE_LIMITER`), so it may already
  be inconsistent across instances behind a load balancer; a rolling
  restart clears all of them.
- **Connecting from a different source IP** also sidesteps the specific
  bucket, since keying is by IP — useful if a shared NAT/proxy IP is what
  tripped the limiter for a legitimate admin alongside unrelated traffic.
- If a genuine incident requires disabling enforcement entirely without a
  restart, set `ADMIN_AUTH_RATE_LIMIT_MODE=shadow` and redeploy/reload
  config per this service's normal deployment process — there is currently
  no runtime (no-redeploy) toggle; adding one is a reasonable follow-up if
  this becomes a repeated operational need, but isn't included here to
  keep this change's scope to what the tracked issue asks for.
