# CI Test File Coverage

## The gap this documents

Before this was fixed, this repository's two test jobs were:

- `unit-tests`: `cargo test --lib --bins` — never builds anything under
  `tests/`.
- `integration-tests`: `cargo test -- --ignored` — builds everything, but
  `--ignored` runs *only* tests marked `#[ignore]`. A non-ignored test living
  in a `tests/*.rs` file was silently skipped: built, but never executed.

The result: a test file could compile, look complete, and pass every time
someone ran it manually, while having zero execution history in CI. This is
exactly how `tests/auth_and_signature_integration_test.rs` (real
`admin_auth` middleware coverage) and `tests/resource_limits_active_tasks_test.rs`
(regression coverage for the `active_tasks()` inversion bug) went unrun —
and, discovered while fixing those, eighteen other files with non-ignored
tests had the same problem:

```
tests/ci_session_management_test.rs
tests/circuit_breaker_test.rs
tests/connection_draining_test.rs
tests/encryption_test.rs
tests/fixtures.rs
tests/health_check_test.rs
tests/ip_filter_integration_test.rs
tests/load_validation_test.rs
tests/metrics_test.rs
tests/migration_tests.rs
tests/query_cache_test.rs
tests/readiness_unit_test.rs
tests/request_logger_test.rs
tests/scheduler_test.rs
tests/settlement_toctou_race_test.rs
tests/telemetry_error_handling_test.rs
tests/webhook_auth_test.rs
tests/webhook_delivery_test.rs
tests/webhook_test.rs
```

All of these were run locally against real Postgres/Redis
(`cargo test --tests --no-fail-fast`) as part of closing this gap. Every one
passed except `tests/settlement_toctou_race_test.rs`, which had a genuinely
stale assertion (`("pending_review", "pending_review")` expected invalid,
contradicting `is_valid_transition`'s documented and separately-tested
same-state-is-idempotent behavior) — fixed alongside this change, since
leaving a real assertion failure in newly-wired CI isn't an option.

## The fix

`.github/workflows/rust.yml`'s `integration-tests` job now has a
**"Run non-ignored tests across all test binaries"** step that runs a plain,
unrestricted `cargo test` — no `--lib`/`--bins` scoping, no `--ignored`
filter. This builds and runs every target (lib, bins, every file in
`tests/`) and executes every test not marked `#[ignore]`. It requires no
per-file allowlist and needs no maintenance when new test files are added —
that's the point: the previous gap existed because coverage depended on
someone remembering to reference each new file explicitly.

## Regression guard

`scripts/check-test-file-ci-coverage.sh` (run as a CI step) fails the build
if this protection is ever narrowed back into blind spots — e.g. if a future
edit changes the wildcard `cargo test` step to a scoped one. It looks for at
least one unrestricted `cargo test` invocation across
`.github/workflows/*.yml`; if it doesn't find one, it falls back to
requiring every `tests/*.rs` file be referenced by an explicit
`--test <name>` flag somewhere, or listed below as an intentional exclusion,
and fails listing whatever's missing.

## Intentionally excluded test files

None today — every file in `tests/` is covered by the wildcard `cargo test`
step described above. If a test file is ever added that legitimately can't
run in CI (e.g. it requires infrastructure unavailable in the GitHub Actions
runner), list its stem here with a reason, so
`scripts/check-test-file-ci-coverage.sh`'s fallback path (and any future
reader) can tell "deliberately excluded" apart from "silently forgotten."

| Test file stem | Reason excluded |
|---|---|
| _(none)_ | |
