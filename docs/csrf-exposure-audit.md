# CSRF Exposure Audit

Enumeration of every route that performs a state-changing (non-GET)
operation, how it authenticates, and whether a browser cookie is involved.
Issue #1310. Audited on `main` at `5a770bc` (router: `create_app` in
`src/lib.rs`).

The cookie part of this audit is enforced by
`src/security/csrf_audit.rs` (`cargo test --lib`), which fails as soon as any
code in `src/` reads or sets a cookie.

## Result

**No route accepts cookie-based authentication, so no route is CSRF-exposed.**
Nothing in `src/` reads the `Cookie` header, sets `Set-Cookie`, or uses a
cookie jar or `SameSite`. `src/security/session.rs` validates session
structs but is not bound to a cookie.

Every state-changing route authenticates with an **explicit per-request
credential that a browser never attaches on its own**:
- `Authorization: Bearer <admin key>` for the admin surface (`admin_auth`);
- an HMAC signature over the body for inbound anchor calls
  (`webhook_signature::verify_anchor_signature`).

A cross-site page can make the browser send a request, but it cannot make it
carry either credential, so a forged request is rejected. Setting a custom
header from another origin also requires a CORS preflight, and CORS
(`src/main.rs`) is off unless `CORS_ALLOWED_ORIGINS` is set, and then allows
only the listed origins.

## State-changing routes

| Method | Path | Authentication | Cookie |
|---|---|---|---|
| POST | `/callback`, `/callback/transaction` (also under `/api/v1`, `/api/v2`) | HMAC signature + IP allowlist | No |
| POST | `/webhook` (also under `/api/v1`, `/api/v2`) | HMAC signature | No |
| PATCH | `/admin/transactions/bulk-status` | `admin_auth` (Bearer) | No |
| POST | `/graphql` | `admin_auth` (Bearer) | No |
| PUT | `/admin/quotas/:tenant_id` | `admin_auth` (Bearer) | No |
| DELETE | `/admin/quotas/:tenant_id/reset` | `admin_auth` (Bearer) | No |
| POST | `/admin/tenants/:tenant_id/rotate-secret` | `admin_auth` (Bearer) | No |
| POST | `/admin/tenants/:tenant_id/revoke-secret` | `admin_auth` (Bearer) | No |
| POST | `/admin/locks/:resource/force-release` | `admin_auth` (Bearer) | No |
| POST | `/admin/compliance/reports` | `admin_auth` (Bearer) | No |
| PATCH | `/admin/settlements/:id/status` | `admin_auth` (Bearer) | No |
| POST | `/admin/reconciliation/run` | `admin_auth` (Bearer) | No |
| PUT, DELETE | `/admin/webhooks/endpoints/:id/filter-rules` | `admin_auth` (Bearer) | No |
| POST | `/admin/webhooks/filter-rules/validate` | `admin_auth` (Bearer) | No |

Defined but not mounted anywhere (unreachable): `webhook_replay_routes()`,
`dlq_routes()` and `admin_routes()` in `src/handlers/`. The former
`/reconnect` routes were removed from the router on `main`.

## Separate finding (not CSRF, out of scope for #1310)

**The CORS configuration panics at startup.** When `CORS_ALLOWED_ORIGINS` is
set, `main.rs` builds `CorsLayer` with `allow_credentials(true)` plus
`AllowMethods::any()` / `AllowHeaders::any()`. tower-http 0.4.4 asserts
against that combination ("Cannot combine
`Access-Control-Allow-Credentials: true` with
`Access-Control-Allow-Headers: *`"), so the server panics while building the
router. This was reproduced with a unit test that applies the same layer.
Recorded here, not fixed.

## If cookie authentication is added

Credentialed CORS is already configured, so cookie auth must ship with its
CSRF defence in the same change:

- `SameSite=Strict` on the session cookie where the admin UI is same-site,
  `Lax` only if top-level cross-site navigation must stay logged in.
- For any cookie-authenticated state-changing route reachable cross-site, add
  a synchronizer token or a strict `Origin` / `Sec-Fetch-Site` check.
- Add a cross-site request test per route, update this document, and
  update the guard in `src/security/csrf_audit.rs`.
