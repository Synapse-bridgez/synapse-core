# Auth rate-limiting architecture

## What exists

`AuthRateLimiter` (`src/auth/rate_limiting.rs`) is a per-identity
token-bucket rate limiter, built for the Vault-integration auth module. It
is fully unit-tested, including a documented fix for an unbounded
bucket-growth bug in its own eviction logic. Before this fix it was
constructed exactly once, in `src/secrets.rs`, and used only for
Vault-probe rate limiting (`check_vault_probe_rate_limit`) — its
per-identity `check_auth_rate_limit` method, the one meant for gating
*authentication attempts themselves*, had no callers anywhere.

The two live authentication middlewares — `middleware::auth::admin_auth`
(guards every `/admin/*` route) and `middleware::auth::api_key_auth`
(currently unwired to any route, but kept consistent so wiring it up later
doesn't reintroduce this gap) — never referenced `AuthRateLimiter`. Neither
did the tenant API-key resolution in `TenantContext`
(`src/tenant/mod.rs`) or `/ws` token authentication
(`handlers::ws::authenticate_ws_token`), both of which check credentials
the same way (`tenants.api_key` lookup) and are, after the fix in this
same change, the actual live tenant-credential-checking paths.

It's an easy mechanism to miss precisely because it's well-built: finding
a complete, unit-tested `AuthRateLimiter` type creates the impression that
"auth rate limiting" as a category is handled somewhere in this codebase —
exactly backwards from the truth.

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
