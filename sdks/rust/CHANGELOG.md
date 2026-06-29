# Changelog

All notable changes to `synapse-sdk` (Rust) will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows the policy described in [VERSIONING.md](./VERSIONING.md).

## [Unreleased]

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
