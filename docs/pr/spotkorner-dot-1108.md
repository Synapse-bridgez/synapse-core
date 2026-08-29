# Pull Request

## Description

Defines the Phase 2 swap-engine service boundary per #1108: a `swap_ready`
transaction state, the data contract handed off at that state, and the trait a
swap-engine implementation must satisfy - with an inert stub. No swap or
DEX-routing logic.

## Related Issue

Closes #1108

## Type of Change

- [x] New feature (non-breaking change which adds functionality)

## Changes Made

- **`src/swap/mod.rs`** (new):
  - `SwapHandoff` - the exact data a `completed` Phase 1 transaction hands to
    Phase 2 (`transaction_id`, `tenant_id: Option<Uuid>`, `stellar_account`,
    `source_asset`, `amount`, `completed_at`, `idempotency_key`, `metadata`), with
    `from_transaction(&Transaction, tenant_id)`.
  - `#[async_trait] trait SwapEngine: Send + Sync` -
    `on_swap_ready(SwapHandoff) -> Result<SwapOutcome, SwapError>` + `name()`.
    Transport-agnostic (in-process / microservice / on-chain contract); must be
    idempotent on `idempotency_key`.
  - `SwapOutcome` (`Skipped` / `Deferred { retry_after_secs }` / `Routed { swap_id }`),
    `SwapError` (`Unavailable` / `InvalidHandoff` / `Internal`, `impl Error`).
  - `NoopSwapEngine` - inert stub, returns `SwapOutcome::Skipped` for every hand-off.
  - `swap_enabled_for(&FeatureFlagService, tenant_id)` - `false` unless the
    `swap_engine` feature flag is on for the tenant (off by default), so nothing is
    opted in.
- **`src/validation/state_transitions.rs`**: added `completed → swap_ready` and
  `swap_ready → failed` to `TRANSACTION_TRANSITIONS`. No Phase 1 code performs the
  `completed → swap_ready` transition, so `pending → processing → completed` is
  unchanged. `test_transaction_transitions_coverage` updated.
- **`src/lib.rs`**: `pub mod swap;`.
- **`docs/state-machine.md`**: new `### swap_ready` state, a "Phase 2 hand-off"
  section documenting `SwapHandoff` / `SwapEngine`, mermaid diagram and transition
  table rows.

## Out of Scope

- Any actual swap / DEX-routing logic.
- Wiring `NoopSwapEngine` into `src/services/transaction_processor.rs` - the issue
  asks for the stub only, and the change must not alter Phase 1 behaviour.

## Testing

- [x] Unit tests added (`src/swap/mod.rs`): `NoopSwapEngine` returns `Skipped`;
  `SwapError` display; `swap_ready` reachable only from `completed`, and
  `completed → swap_ready` / `swap_ready → failed` valid while the reverses are not.
- [x] `cargo test -p synapse-core --lib` passes.
- [x] All existing tests pass - the added transitions are additive.

## Migration Safety (if applicable)

Not applicable - no database migration. The `swap_ready` value is a new logical
status; `transactions.status` is `VARCHAR(20)` with no CHECK constraint, so no
schema change is required for a future hand-off step to write it.

## Checklist

- [x] My code follows the style guidelines (CONTRIBUTING.md)
- [x] I have performed a self-review
- [x] I have commented my code, particularly the hand-off contract
- [x] I have made corresponding changes to the documentation (`docs/state-machine.md`)
- [x] My changes generate no new warnings
- [x] I have added tests that prove the feature works
- [x] New and existing unit tests pass locally with my changes

## Pre-Submission Checks

- [x] `cargo fmt --all -- --check` passes
- [x] `cargo clippy -- -D warnings` passes for the new/touched code
- [x] `cargo build` succeeds
- [x] `cargo test` passes

## Additional Context

Branch targets `main`, matching every recent merged PR despite CONTRIBUTING.md
mentioning `develop`. This PR closes #1108 only; the sibling database issues
(#1110 partition retention, #1111 query-plan CI, #1112 index-migration tooling)
are separate efforts.

Closes #1108
