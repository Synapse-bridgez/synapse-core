# Changelog

All notable changes to `synapse-sdk` (Rust) will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows the policy described in [VERSIONING.md](./VERSIONING.md).

## [Unreleased]

### Changed

- **Breaking:** `SynapseClient::post` and `AdminSynapseClient::post` no
  longer auto-retry on transient failures (network errors, 5xx). Retrying a
  mutating request whose response was lost could silently resend it as a
  duplicate (e.g. a webhook replay delivered twice, a reconciliation run
  started twice). If your application relied on the previous auto-retry
  behavior for POST, wrap the call in your own retry loop with an
  idempotency key. GET requests are unaffected and continue to retry as
  before.
- The retry layer now honors a server-sent `Retry-After` header (seconds)
  on transient responses, using it instead of client-side jitter when
  present.
- `SynapseError::RateLimited` now carries the 429 response body message
  (previously discarded) as `RateLimited(String)`.

### Fixed

- **Breaking (bug fix):** `AdminSynapseClient` sent admin requests with an
  `X-Admin-Key` header, but the server's `admin_auth` middleware only ever
  checks `Authorization: Bearer <token>` — every request made through
  `AdminSynapseClient` (`dlq()`, `webhook_replay()`, `reconciliation()`,
  `settlements()`, `locks()`, `bulk_status()`) failed with `401` against any
  real server, regardless of the admin key supplied. It now sends
  `Authorization: Bearer <admin_key>`. Any code that worked around this by
  calling the server directly with the correct header should switch back to
  `AdminSynapseClient` now that it authenticates correctly.

### Added

- Initial crate scaffold: `Cargo.toml`, `src/lib.rs`, public module layout.
- HTTP client wrapper (`SynapseClient`) with configurable base URL and auth token.
- Core domain models (`synapse`, `event`, `subscription`) with `serde` derive support.
- `AdminClient` for reconciliation-report endpoints (list, get, trigger reconcile).
- `ReconciliationReport` and `ReconciliationStatus` types matching the REST contract.
- Pagination helpers (`PageParams`, `PagedResponse<T>`).
- Retry / back-off logic via `tokio::time` for transient 5xx responses.
- Integration test harness using `wiremock` for HTTP mocking.
- `examples/` directory with a minimal end-to-end usage example.
- Wired into the root workspace (`Cargo.toml` `[workspace] members`).
- Scoped CI workflow (`.github/workflows/sdk-rust-ci.yml`): fmt, clippy, tests.
