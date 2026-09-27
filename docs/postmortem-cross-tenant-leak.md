# Postmortem: unauthenticated cross-tenant data exposure

**Status:** Fixed in this change. Some follow-up items require production access this
repository doesn't have — see "Known gaps" at the end.

## Summary

`GET /transactions`, `GET /transactions/:id`, `GET /transactions/search`,
`GET /settlements`, `GET /settlements/:id`, and `/ws` resync returned real,
complete, cross-tenant financial data to callers who should not have had
access to it: the first five to *any* caller, with no credentials required
at all; the sixth to any caller holding a valid API key for *any single*
tenant.

This was not one bug. It was three independent, compounding failures, each
individually well-built and each invisible from inside its own file:

1. **Missing route auth.** `core_routes` (`src/lib.rs`) was built on a bare
   `Router::new()` with no `.layer()` of its own. The only middleware ever
   applied to it was a version-header stamp.
2. **Unenforced RLS.** `migrations/20260501000000_tenant_rls.sql` is
   correct, forced Row-Level Security SQL. It provided zero protection
   because every environment this repo defines (`docker-compose.yml`,
   `docker-compose.dev.yml`, CI) connected the app itself as the Postgres
   `initdb` bootstrap superuser role, and superusers unconditionally bypass
   RLS (`rolbypassrls = true` on that specific role — verified directly
   against a real Postgres instance; this is *not* inherited by roles that
   role subsequently creates, which is why the fix is a genuinely separate,
   non-superuser role rather than a flag on an existing one).
   `TenantContext`, `queries::with_tenant`, and
   `tenant_context_middleware` all existed, correctly written, with zero
   live callers.
3. **Unfiltered WS resync.** `lookup_api_key` returned a `bool`, never which
   tenant matched. `ws_handler` didn't even call it — it only format-checked
   the token — so resync had no tenant identity to filter by in the first
   place, independent of (1) and (2).

## Why this was hard to see

Every individual piece was well-built in isolation. The RLS policy is
correct SQL. `TenantContext` is correct Rust. `core_routes`'s missing
`.layer()` call looks like a normal, easy-to-miss omission on its own. None
of those, read in isolation, looks like a critical vulnerability — the
compounding failure only shows up when you ask "which Postgres role does
the deployed application actually run as, and does that role's privileges
undermine a SQL-level policy regardless of what the policy says?" That
question sits in `docker-compose.yml`/CI config, outside the Rust codebase
and outside any single migration file. A review of `tenant.rs`,
`middleware/tenant.rs`, and the RLS migration in isolation would reasonably
— and wrongly — conclude the policy provided some protection.

## The fix (this change)

- **Route auth**: `TenantContext` (a `FromRequestParts` extractor, now
  generic over any state `S: FromRef<AppState>` so it works in both
  `AppState`- and `ApiState`-keyed routers) is now the live extractor on
  `GET /transactions`, `/transactions/:id`, `/transactions/search`,
  `/settlements`, `/settlements/:id`. It resolves tenant identity from a
  looked-up API key and rejects anything else — see "Also fixes" below for
  a bug found while wiring it in.
- **RLS enforcement**: `scripts/db-init/01-create-app-role.sql` provisions a
  `synapse_app` role with explicit `NOBYPASSRLS`; docker-compose files and
  CI now connect as it instead of the bootstrap superuser.
  `src/startup.rs::assert_no_bypassrls` hard-fails process startup if the
  connected role can bypass RLS, so a misconfigured `DATABASE_URL` refuses
  to start rather than silently leaking data. Every pooled connection now
  defaults to `app.is_admin = true` at the session level
  (`db::set_session_admin_context`) so the rest of the app (webhook
  ingestion, settlement batch jobs, partition management, admin bulk
  operations) keeps its existing full-visibility behavior; the five
  customer-facing endpoints above narrow that per-request via
  `queries::with_tenant`'s `SET LOCAL`, which auto-clears on
  commit/rollback and can't leak across pooled connections.
- **Settlements**: settlements aggregate transactions *across* tenants by
  `asset_code` (`SettlementService::settle_asset` doesn't group by tenant),
  so a `tenant_id` column would misrepresent the data. Visibility is scoped
  by an `EXISTS` join against `transactions.tenant_id` instead — both at the
  query layer and in a new RLS policy
  (`migrations/20260824000001_settlement_rls.sql`). See that migration's
  comment for the full reasoning; this is a deliberate deviation from the
  literal "add a tenant_id column" suggestion.
- **WS resync**: `lookup_api_key` now returns `Option<Uuid>` (which tenant
  matched). `ws_handler` calls it (or checks the admin key) before
  upgrading the connection at all — previously it never checked the token
  against anything real — and threads the resolved identity through to the
  `Resync` handler, which now filters by tenant.

## Also fixes (found while wiring the above, not in the original issue text)

- **`admin_auth` had zero callers anywhere in the router.** Every
  `/admin/*` route, plus `/graphql`, `/export`, `/stats/*`, and
  `/cache/metrics`, was reachable with no credentials at all — worse than
  this issue's Part C described, which assumed `admin_auth` already covered
  these and was only missing rate limiting. `admin_auth` is now applied to
  all of them; `/live`, `/ready`, `/health`, `/errors` stay public
  (infra health probes have no credentials to send).
- **`api_key_auth` had zero callers anywhere in the router**, contrary to
  this issue's evidence section, which states `callback_routes`/
  `webhook_routes` "correctly layer api_key_auth." They don't — those two
  routes authenticate via HMAC signature validation
  (`validate_callback`/`validate_webhook`), which is untouched and correct
  for inbound anchor webhooks; `api_key_auth` itself was simply never
  wired to anything. It's still not wired to a route (nothing needs a
  boolean-only auth check once `TenantContext` covers the tenant-scoped
  routes), but it now has rate limiting applied so it doesn't reintroduce
  this gap if someone wires it up later.
- **`TenantContext` trusted a bare `X-Tenant-ID` header as sufficient proof
  of tenant identity**, with no credential check at all. Wiring the
  extractor onto real routes as-is would have traded "leaks to anyone" for
  "leaks to anyone who guesses a tenant UUID." That fallback is removed;
  only a looked-up API key resolves tenant identity now.
- **`resolve_tenant_id` tried `Path<Uuid>` first**, which on a route like
  `/transactions/:id` consumed the *transaction* ID as if it were the
  tenant ID and returned early, before ever checking for an API key — every
  request would have failed tenant lookup regardless of whether a valid
  credential was supplied. Removed; nothing legitimately depended on it.
- **`/ws` had no real authentication of any kind before this fix** — not
  even the single-tenant-key requirement the issue describes.
  `validate_ws_token` only checked token *format* (non-empty, under 1024
  bytes, no null bytes); any syntactically-plausible string opened a
  connection. This is fixed as part of the same change that adds tenant
  filtering, since both required actually checking the token against the
  `tenants` table.
- **`/reconnect/status` and `/reconnect` were removed entirely** rather
  than patched (auth + a cleanup schedule). They were unauthenticated, grew
  an in-memory session map without bound, and — the deciding factor —
  were never consulted by `ws_handler` at all; a client calling them got a
  session_id and backoff recommendation with zero bearing on its real
  WebSocket connection. Securing a subsystem that's disconnected from the
  thing it claims to support would have been misleading, not fixed.
- **The new settlement RLS policy's `EXISTS` clause against `transactions`
  creates a real Postgres dependency** between the two tables — `DROP TABLE
  transactions` without `CASCADE` now fails
  ("policy tenant_isolation on table settlements depends on table
  transactions"). Caught by `tests/settlement_test.rs::test_settle_error_handling`,
  which intentionally drops the table to force a database error; fixed with
  `CASCADE`, which is what that test actually needs.
- **`#[sqlx::test]` (`tests/webhook_replay_test.rs`) needs its connecting
  role to `LOCK TABLE pg_catalog.pg_namespace`** while provisioning its own
  ephemeral per-test database — a catalog-level lock only a superuser can
  reliably take, and not something `CREATEDB` alone resolves. Converted
  that file to the same testcontainers-per-test pattern the rest of this
  suite uses, rather than widening `synapse_app`'s privileges for one test
  file.

## Blast radius

Three independent surfaces, assessed separately as the issue requests:

1. **REST routes** (`/transactions*`, `/settlements*`): any caller, any
   time, no credentials — the widest exposure. Access logs (outside this
   repo) should be checked for GET requests to these five paths with no
   `X-API-Key`/`Authorization` header, or with a key that doesn't belong to
   the tenant whose data was returned.
2. **RLS bypass**: relevant only to callers who *did* have some path to a
   direct database connection (e.g. an internal tool, a debugging session)
   using the bootstrap role — narrower than (1) but not visible from
   application logs at all, since it never went through the app layer.
3. **`/ws` resync**: bounded to callers holding a valid API key for *some*
   tenant — narrower than (1), but still cross-tenant. Access logs should
   be checked for `/ws` connections whose subsequent resync response
   contained transactions outside the authenticating tenant's own
   `tenant_id`.

## Known gaps

These require access this repository/checkout does not have, and are
named explicitly rather than left implied as "done":

- **Production data audit.** Rows written during the exposure window with
  `tenant_id = NULL` are, by design, visible to every tenant under the
  existing "legacy row" RLS convention (see the original RLS migration).
  Whether any of those rows are genuinely cross-tenant-sensitive (as
  opposed to legitimately pre-migration data) requires querying the actual
  production `transactions`/`settlements` tables, which this repo has no
  access to. **Action for whoever has that access:** run
  `SELECT count(*), min(created_at), max(created_at) FROM transactions
  WHERE tenant_id IS NULL` and cross-reference the date range against when
  each of the three failures above was introduced (via `git blame` on the
  relevant migrations/router code) to bound how much of that NULL set is
  "expected legacy" vs. "written during the live exposure window." Rows
  that can't be attributed to a tenant after the fact are, by definition,
  unrecoverable — document them as such rather than guessing.
- **Access log review.** The blast-radius breakdown above describes *what*
  to look for; actually running that search requires the real access
  logs / APM data for this service, which this repo does not have.
- **Verifying no legitimate integration depends on the old unauthenticated
  behavior**, before this fix is rolled out enforcing (no gradual
  rollout, per the issue's own urgency call) — also requires production
  traffic visibility this repo doesn't have.
- **Real Postgres role provisioning for production.** This change ships
  `scripts/db-init/01-create-app-role.sql` (docker-compose) and inline CI
  steps, and hard-fails startup via `assert_no_bypassrls` if production's
  actual role still bypasses RLS. It cannot itself provision the
  production role — whoever manages that database must create it (see the
  script for the exact grants) before deploying this change, or the
  service will refuse to start.
