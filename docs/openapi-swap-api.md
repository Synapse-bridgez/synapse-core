# OpenAPI 3.1 Specification & Generated API Reference for Swap Endpoints

Status: **partially implemented** — this document is the plan of record for
issue #1463. The spec, the CI parity check and the generated reference are
described here; the remaining work is listed at the end.

## Problem

The swap surface (`/swap/*`, plus the transaction/settlement/webhook routes it
depends on) is documented only in prose (`docs/swap-developer-integration-guide.md`,
`docs/api-reference.md`). There is no machine-readable contract, so:

- client SDKs and integrators hand-write types and drift from the server;
- nothing fails CI when a route is added, renamed or its payload changes;
- error envelopes, idempotency headers, auth, pagination and rate-limit headers
  are described inconsistently across docs.

## Approach

1. **Author one versioned OpenAPI 3.1 document** at
   `docs/openapi/swap-v1.yaml` (`openapi: 3.1.0`, `info.version: 1.0.0`).
   It must cover, at minimum:
   - `POST /swap/quote` — request/response schema, `Idempotency-Key` header.
   - `POST /swap/orders` — order creation, `Idempotency-Key`, `201` + `Location`.
   - `DELETE /swap/orders/{id}` — cancellation, `409` on non-cancellable state.
   - `GET /swap/orders/{id}` — status lookup.
   - `GET /swap/orders` — cursor pagination (`cursor`, `limit`, `direction`),
     `next_cursor`/`has_more` in the response.
   - Lifecycle webhooks (`POST /webhook`, `POST /callback`,
     `POST /callback/transaction`) with the HMAC signature header
     (`X-Stellar-Signature`) and the `WebhookResponse` envelope.
   - Shared components: `ErrorEnvelope` (the shape produced by
     `src/middleware/error_enrichment.rs` — `error`, `code`, `request_id`),
     `Idempotency-Key` header parameter, `X-RateLimit-Limit` /
     `X-RateLimit-Remaining` / `X-RateLimit-Reset` / `Retry-After` response
     headers, `securitySchemes` for tenant API key (`X-API-Key`) and admin auth.
2. **CI parity check** — `scripts/check-openapi-parity.sh`:
   - extracts every `(method, path)` pair from `src/lib.rs` route registrations
     (`Router::new().route(...)`, `.nest(...)` prefixes) with a small
     `grep`/`awk` pass, normalising `:param` → `{param}`;
   - extracts every `(method, path)` pair from the OpenAPI document;
   - fails on any route present in one and not the other, and on any
     `#[derive(ToSchema)]` type referenced by a documented operation that no
     longer exists in `src/`;
   - supports `--spec <file>` so the fixture test below can point it at a
     mutated copy.
3. **Fixture test** — `scripts/fixtures/openapi/mutated-route.yaml` is a copy of
   the spec with one route renamed (`/swap/quote` → `/swap/quotes`).
   `scripts/test-openapi-parity.sh` asserts the check exits non-zero on that
   fixture and zero on the real spec.
4. **Generated browsable reference** — `docs/openapi/swap-v1.html`, produced by
   Redocly (`npx @redocly/cli build-docs docs/openapi/swap-v1.yaml -o
   docs/openapi/swap-v1.html`) and committed. A smoke test
   (`scripts/test-openapi-reference.sh`) runs `npx @redocly/cli lint` and fails
   on any warning, then asserts the HTML file exists and is non-empty.
5. **Versioning policy** — documented in `docs/openapi/README.md`:
   - additive changes (new optional fields, new endpoints) → minor bump;
   - removing/renaming a field, tightening validation, changing a status code
     → major bump, new file `swap-v2.yaml`, old file kept for one deprecation
     window with `deprecated: true` on affected operations;
   - the `info.version` in the spec must match the `API-Version` header emitted
     by `src/middleware/versioning.rs`.

## Files to change

| File | Change |
| --- | --- |
| `docs/openapi/swap-v1.yaml` | new — the OpenAPI 3.1 document |
| `docs/openapi/swap-v1.html` | new — generated browsable reference |
| `docs/openapi/README.md` | new — versioning policy + regeneration instructions |
| `scripts/check-openapi-parity.sh` | new — route/schema parity check |
| `scripts/test-openapi-parity.sh` | new — fixture test for the check |
| `scripts/test-openapi-reference.sh` | new — lint + build smoke test |
| `scripts/fixtures/openapi/mutated-route.yaml` | new — intentionally mutated spec |
| `.github/workflows/ci.yml` | add the three scripts as CI steps |

## Steps in order

1. Enumerate the live routes from `src/lib.rs` (`create_app`) and the handler
   modules; write them into `docs/openapi/swap-v1.yaml` with schemas taken from
   the `ToSchema`/`Serialize` structs in `src/handlers/`.
2. Write `scripts/check-openapi-parity.sh` and run it against the spec until it
   passes.
3. Copy the spec to `scripts/fixtures/openapi/mutated-route.yaml`, rename one
   route, and write `scripts/test-openapi-parity.sh` asserting the failure.
4. Generate `docs/openapi/swap-v1.html` with Redocly and commit it.
5. Write `scripts/test-openapi-reference.sh` (lint with `--format stylish` and
   fail on warnings; assert the HTML exists).
6. Write `docs/openapi/README.md` with the versioning policy.
7. Wire all three scripts into CI.

## Done when

- `docs/openapi/swap-v1.yaml` validates as OpenAPI 3.1 with zero lint warnings.
- `scripts/check-openapi-parity.sh` exits 0 on the committed spec and non-zero
  on `scripts/fixtures/openapi/mutated-route.yaml`.
- `docs/openapi/swap-v1.html` is committed and opens as a browsable reference.
- The versioning policy is documented and referenced from `docs/api-reference.md`.

## What is left

Nothing from the list above has been committed yet: the spec, the scripts, the
fixture, the generated HTML and the CI wiring all still need to be written.
This document is the agreed plan; the parity check is the piece to build first
because it defines the exact route list the spec must contain.
