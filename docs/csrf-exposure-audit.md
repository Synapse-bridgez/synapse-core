# CSRF Exposure Audit

Enumeration of every route that performs a state-changing (non-GET)
operation, how it authenticates, and whether a browser cookie is involved.
Issue #1310. Audited on `develop` at `c1cacec` (router: `create_app` in
`src/lib.rs`).

The cookie part of this audit is enforced by
`src/security/csrf_audit.rs` (`cargo test --lib`), which fails as soon as any
code in `src/` reads or sets a cookie.

## Result

**No route accepts cookie-based authentication, so no route is CSRF-exposed
through an ambient browser credential.** Nothing in `src/` reads the
`Cookie` header, sets `Set-Cookie`, or uses a cookie jar or `SameSite`.
`src/security/session.rs` validates session structs but is not bound to a
cookie.

Two further properties would each also block a classic cross-site form
post:

- Every state-changing handler takes an axum `Json<_>` body, which rejects
  anything but `Content-Type: application/json` (415). An HTML form cannot
  send that content type, and a cross-site `fetch` with it triggers a CORS
  preflight.
- CORS (`src/main.rs`) is off unless `CORS_ALLOWED_ORIGINS` is set, and
  then allows only the listed origins.

## State-changing routes

| Method | Path | Mounted with | Authentication | Cookie |
|---|---|---|---|---|
| POST | `/callback`, `/callback/transaction` (also under `/api/v1`, `/api/v2`) | IP allowlist, schema validation, quota | IP allowlist only | No |
| POST | `/webhook` (also under `/api/v1`, `/api/v2`) | schema validation, quota | **None** | No |
| PATCH | `/admin/transactions/bulk-status` | panic recovery | **None** | No |
| PUT | `/admin/quotas/:tenant_id` | panic recovery | **None** | No |
| DELETE | `/admin/quotas/:tenant_id/reset` | panic recovery | **None** | No |
| PATCH | `/admin/settlements/:id/status` | panic recovery | **None** | No |
| POST | `/admin/reconciliation/run` | panic recovery | **None** | No |
| POST | `/graphql` | panic recovery | **None** | No |
| POST | `/reconnect` | none | **None** | No |

Defined but not mounted anywhere (unreachable): `webhook_replay_routes()`
(`POST /webhooks/replay/:id`, `POST /webhooks/replay/batch`,
`POST /webhooks/endpoints/:id/rate-limit`) and `dlq_routes()`
(`POST /dlq/:id/requeue`).

## Separate findings (not CSRF, out of scope for #1310)

These came up while enumerating authentication. They are recorded here, not
fixed.

1. **The admin surface is unauthenticated.** `docs/api-reference.md` states
   that `/admin/*` requires `Authorization: Bearer <ADMIN_API_KEY>`, but
   neither `middleware::auth::admin_auth` nor `api_key_auth` is layered on
   any route, and none of the handlers above extract a credential. Anyone who
   can reach the service can change transaction and settlement status, set or
   reset tenant quotas, trigger reconciliation, and call `/graphql`. CSRF
   protection is moot until these routes require a credential.
2. **Webhook signatures are not verified.** `handlers::auth::VerifiedWebhook`
   (HMAC `X-Stellar-Signature`) has no callers, so `/webhook` accepts any
   well-formed body, contrary to the API reference.
3. **The CORS configuration panics at startup.** When `CORS_ALLOWED_ORIGINS`
   is set, `main.rs` builds `CorsLayer` with `allow_credentials(true)` plus
   `AllowMethods::any()` / `AllowHeaders::any()`. tower-http 0.4.4 asserts
   against that combination ("Cannot combine
   `Access-Control-Allow-Credentials: true` with
   `Access-Control-Allow-Headers: *`"), so the server panics while building
   the router. This was reproduced with a unit test that applies the same layer.

## If cookie authentication is added

Credentialed CORS is already configured, so cookie auth must ship with its
CSRF defence in the same change:

- `SameSite=Strict` on the session cookie where the admin UI is same-site,
  `Lax` only if top-level cross-site navigation must stay logged in.
- For any cookie-authenticated state-changing route reachable cross-site, add
  a synchronizer token or a strict `Origin` / `Sec-Fetch-Site` check.
- Add a cross-site request test per route, update this document, and
  update the guard in `src/security/csrf_audit.rs`.
