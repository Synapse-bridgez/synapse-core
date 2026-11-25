# ADR-007: Remove Orphaned Hexagonal Transaction Layer and Payments Module

## Status

Accepted

## Context

Two subsystems existed in the tree with no live callers anywhere:

1. **Hexagonal transaction layer** — `src/domain/transaction.rs`, `src/ports/transaction_repository.rs`,
   `src/adapters/postgres_transaction_repository.rs`, `src/use_cases/process_deposit.rs`. These were not
   even declared as modules in `src/lib.rs` (no `mod domain;` / `mod ports;` / `mod adapters;` / `mod use_cases;`
   anywhere in the tree), so they did not compile into the crate at all. None of the four files contained a
   single test.
2. **`src/payments/` module** — a hand-rolled connection pool, error types, CSV/JSON export, and pagination
   surface for "settlement logic." Unlike the hexagonal layer, this one *was* declared (`pub mod payments;`
   in `src/lib.rs`) and compiled, and it was extensively unit-tested (~94 tests across its five files). But
   `grep -rn "payments::" src/ tests/` outside `src/payments/` itself returned zero matches — nothing outside
   the module ever called into it.

The live transaction-write path is `src/db::queries::insert_transaction`, called from `src/handlers/webhook.rs`
(the real anchor callback/webhook handlers). It has never gone through `ProcessDeposit` or
`PostgresTransactionRepository`. Likewise, `src/payments::connection_pool` duplicates the responsibility of
the already-live `src/db::pool_manager`; `src/payments::export` duplicates `src/handlers/export.rs`; and
`src/payments::pagination`/`input_validation` duplicate pagination and validation logic already used by the
live transaction/settlement handlers. Nothing about either subsystem closes a capability gap the live code
doesn't already have — both read as an abandoned migration attempt toward a cleaner architecture, not a
dormant feature waiting to be turned on.

This is the same failure mode as ADR-004 (`ReconciliationJob` vs. `AccountMonitor`): two fully-built
systems for one responsibility, one live, one not. ADR-004 resolved that case by keeping the dormant
system in place (with fixes) and documenting authority, because `AccountMonitor` closed a real latency gap
`ReconciliationJob` didn't. Neither the hexagonal layer nor `payments/` has an equivalent unique value —
they are pure duplication with no new capability — so the calculus here is different.

## Decision

**Delete both subsystems entirely.** `src/domain/`, `src/ports/`, `src/adapters/`, `src/use_cases/`, and
`src/payments/` are removed, along with the `pub mod payments;` declaration in `src/lib.rs`. No production
code or test referenced any symbol from these modules, so the deletion has zero runtime behavior change.

This is a deliberate large deletion, not a partial fix — leaving either subsystem in place (compiled or not)
would continue to bait a future engineer into building on top of code that never executes, or wiring it in
without realizing `db::queries`/`db::pool_manager`/`handlers::export` already do the same job live.

## Consequences

### Positive

- Removes ~1,760 lines of code that looked production-ready but never ran, eliminating the exact "grep
  finds a plausible, tested implementation and assumes it's load-bearing" trap this issue is about.
- One less place to keep in sync with schema/behavior changes to `transactions`/settlements.
- No duplicate pagination/validation/export/connection-pooling logic to accidentally diverge from the live
  implementations.

### Negative

- If a future hexagonal-architecture migration is genuinely wanted, this work is gone and would need to be
  rebuilt. Given it was never wired in and duplicated existing live functionality rather than replacing it,
  the actual design work of *how* to migrate `db::queries` callers to a ports/adapters model was never done
  here anyway — this scaffolding wasn't a usable starting point for that migration, just isolated pieces.

### Neutral

- `docs/`/`README.md` were checked for references to a hexagonal-architecture pattern as the intended
  approach for new features; none exist, so no developer-facing documentation needed updating.

## Alternatives Considered

### Alternative 1: Wire the hexagonal layer in as the new deposit path

**Description:** Add the missing `mod` declarations, construct `ProcessDeposit`/`PostgresTransactionRepository`
in `main.rs`, and migrate `handlers/webhook.rs` to call through them instead of `db::queries::insert_transaction`.

**Cons:** No tests exist for any of the four files, so "wire it in" would mean shipping an untested code path
for live money-movement deposit handling. There's also no design record for why this migration was started or
what its target end-state looks like, so "finishing" it would mean inventing that design now rather than
executing an existing plan.

**Why rejected:** Too large and risky a scope change to bundle with a dead-code cleanup, and there's no
evidence (design doc, partial caller migration, anything) that this was ever more than a first sketch.

### Alternative 2: Keep `payments/` dormant and document it (ADR-004 style)

**Description:** Since `payments/` is compiled and well-tested, leave it in place undeleted and write an ADR
recording it as "authoritatively not live," the way ADR-004 did for `AccountMonitor`.

**Cons:** `AccountMonitor` earned that treatment because it closes a real gap (real-time payment matching)
that the live `ReconciliationJob` path doesn't. `payments/` doesn't close any gap — every one of its five
files duplicates functionality the live code already has (pool_manager, handlers/export.rs, existing
pagination/validation). Keeping duplicate-but-unused code around has no corresponding upside to weigh
against the maintenance/confusion cost.

**Why rejected:** the ADR-004 precedent applies specifically to dormant code with unique value; `payments/`
has none.

## Implementation Notes

- If real-time hexagonal-architecture migration is wanted in the future, start with a design doc that names
  which live `db::queries` callers migrate first and in what order, rather than resurrecting this code —
  it was never validated against the live schema/behavior and predates several since-added columns
  (e.g. `horizon_payment_id`).
- The new CI check added alongside this ADR (`scripts/check-unreachable-modules.sh`) flags any `pub mod`
  with zero non-test, non-self callers after a build, to catch this class of drift before it accumulates
  again.

## References

- `src/db/queries.rs::insert_transaction`, `src/handlers/webhook.rs` — the live path this scaffolding never joined.
- ADR-004 — the precedent this ADR distinguishes itself from.
- Issue: orphaned-code audit, Part A.
