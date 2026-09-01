# Changelog

All notable changes to `synapse-sdk` (Rust) will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows the policy described in [VERSIONING.md](./VERSIONING.md).

## [Unreleased]

### Added

- `ErrorCode` enum giving every code in `docs/error-catalog.md` a distinct,
  matchable SDK variant (e.g. `ErrorCode::Transaction004` for
  `ERR_TRANSACTION_004`), instead of consumers having to parse `message`
  strings to distinguish failure modes. Unrecognized codes degrade to
  `ErrorCode::Unknown(String)` rather than panicking. A new
  `error_catalog_sync_test` fails the build if the catalog documents a code
  with no matching variant.
- `graphql_builder` module: a typed, fluent query builder for the
  `transactions`/`transaction` and `settlements` GraphQL query shapes, so
  common queries don't require hand-written query strings. See
  `examples/graphql_query.rs`.

### Changed

- **Breaking:** `SynapseError::Api` gained a new `code: Option<ErrorCode>`
  field. Existing `match`/`if let` patterns using `..` are unaffected;
  exhaustive field patterns (`Api { status, message }` with no `..`) need to
  add `..` or bind the new field.

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
