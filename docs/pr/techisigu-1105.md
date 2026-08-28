# Pull Request

## Description

Audit of API v1/v2 handler parity and a concrete v1 deprecation plan, per #1105.
Documentation-only change: a new `docs/api-versioning-deprecation.md`.

## Related Issue

Closes #1105

## Type of Change

- [x] Documentation update

## Changes Made

- New `docs/api-versioning-deprecation.md`:
  - **Parity matrix** for every route in `core_routes` (`src/lib.rs`) across `/api/v1`
    and `/api/v2`. Finding: **100% parity** - both prefixes mount the identical
    `core_routes` router; the only difference is v1's `Deprecation` / `Sunset` /
    `API-Version` response headers. No v1-only functionality, no v2 gap-fills needed.
  - Notes that `src/handlers/v1/mod.rs` and `src/handlers/v2/mod.rs` are byte-identical
    dead re-export stubs (declared in `handlers/mod.rs`, referenced nowhere) - flagged
    as a follow-up cleanup, not touched here.
  - **Consumers:** `sdks/rust` and `cli/synapse-cli` are base-URL-driven and
    version-agnostic; external clients pin the version through their base URL.
  - **Deprecation plan and sunset timeline** formalizing the `2026-12-31` sunset already
    advertised by `middleware/versioning.rs`, with announce / reminder / final-notice /
    sunset / removal milestones and client-communication steps.

No code changes: the versions are already at full parity, so the issue's
"trivial v2 gap-fills identified during the audit" clause yields nothing.

## Testing

- [x] All existing tests pass (doc-only; `cargo check --workspace` unaffected)
- `tests/api_versioning_test.rs` remains `#[ignore]`d (needs a live Postgres/Redis); it
  already asserts v1's deprecation headers and v2's absence of them.

## Migration Safety (if applicable)

Not applicable - no database migration.

## Checklist

- [x] My code follows the style guidelines (CONTRIBUTING.md)
- [x] I have performed a self-review
- [x] I have made corresponding changes to the documentation (this PR is the doc)
- [x] My changes generate no new warnings
- [ ] I have added tests - not applicable, documentation-only audit deliverable
- [x] New and existing unit tests pass locally with my changes

## Pre-Submission Checks

- [x] `cargo fmt --all -- --check` passes (no code changed)
- [x] `cargo clippy -- -D warnings` passes (no code changed)
- [x] `cargo build` succeeds
- [x] `cargo test` passes (unchanged)

## Additional Context

Branch targets `main`, matching every recent merged PR (#1080-#1084) despite
CONTRIBUTING.md mentioning `develop`. The issue's other items (#1106 pagination,
#1107 quota API, #1099 Horizon resumption) are separate efforts and not part of this PR.

Closes #1105
