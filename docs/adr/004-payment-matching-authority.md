# ADR-004: Payment-Matching Authority — ReconciliationJob vs. AccountMonitor

## Status

Proposed

## Context

This codebase has two independently-built, fully-implemented mechanisms for matching Stellar Horizon payments against `transactions` rows:

1. **`ReconciliationJob`** (`src/services/reconciliation.rs`) — a scheduled batch job (daily, 02:00 UTC) that queries Horizon for a time window, diffs it against `transactions`, and writes a **report** (`reconciliation_reports`) for a human to review. It is registered with `JobScheduler` in `main.rs` and genuinely runs in production. It never writes to `transactions` — it is read-only/report-only.
2. **`AccountMonitor`** (`src/services/account_monitor.rs`) — a polling/streaming watcher that matches incoming Horizon payments to a candidate pending transaction by memo and **directly completes the transaction** (`UPDATE transactions SET status = 'completed', horizon_payment_id = ...`). It is fully unit-tested, uses `BigDecimal` deliberately to avoid rounding errors, and handles overpayment — but `AccountMonitor::new` / `.start()` is never called anywhere outside its own test module. It is not constructed in `main.rs` and does not run in production today.

Nobody has recorded a decision about which of these two is meant to be the long-term production payment-matching mechanism. Both have real bugs (see the accompanying PR): `ReconciliationJob` could double-insert a report under concurrent execution (now fixed with `LeaderElection` gating + a unique constraint on `(period_start, period_end)`); `AccountMonitor::process_payment` had no row lock across its read-decide-write sequence (now fixed with `SELECT ... FOR UPDATE` + a `WHERE status = 'pending'` guard on the completion write). Fixing either one's concurrency bug in isolation, without deciding which is authoritative, leaves two half-finished systems and an open question about which one operators should trust.

## Decision

**`ReconciliationJob` remains the sole authoritative payment-matching mechanism for now.** It is the one actually running in production, it only ever writes an audit report (never mutates `transactions` directly), and its failure mode is "a human reviews a report a bit late" rather than "a transaction is marked completed incorrectly." `AccountMonitor` stays unconstructed / not wired into any live path.

This PR fixes both mechanisms' concurrency bugs as a safety net (so neither is a landmine for whoever revisits this next), but does **not** wire `AccountMonitor` into `main.rs`. Promoting `AccountMonitor` to live — i.e. letting it autonomously flip `transactions.status` to `completed` based on Horizon payment matching, with no human in the loop — is a materially higher-stakes, live-money-movement change that deserves its own scoped rollout (see Consequences), not a decision made as a side effect of a bug-fix PR.

## Consequences

### Positive

- No live path today can auto-complete a transaction from Horizon data without going through a batch report + human review, so there is exactly one place to look when triaging a payment-matching discrepancy.
- `AccountMonitor`'s now-fixed row-locking discipline means that whenever this decision *is* revisited, wiring it in won't immediately reintroduce a live money-movement race.

### Negative

- Reconciliation remains next-day (batch), not real-time. A payment received today isn't matched and reflected as `completed` until the following 02:00 UTC run (or a manual admin trigger) — `AccountMonitor` was presumably built to close that latency gap and continues to sit unused.
- Two fully-built systems for the same responsibility remain in the codebase, one live, one dormant. That duplication itself is a maintenance cost and a source of future confusion for anyone who finds `AccountMonitor` and assumes it's live.

### Neutral

- This ADR doesn't resolve the duplication, only the immediate safety question (both are now concurrency-safe) and the immediate authority question (only one runs).

## Alternatives Considered

### Alternative 1: Promote AccountMonitor to live now

**Description:** Wire `AccountMonitor::new(...).start()` into `main.rs` alongside or instead of `ReconciliationJob`, so incoming payments are matched and completed in near-real-time.

**Pros:**
- Closes the latency gap between a payment arriving and its transaction being marked completed.
- The code is already written, tested in isolation, and (after this PR) has correct row-locking.

**Cons:**
- This is the first live path that would let Horizon payment data directly flip a transaction to `completed` with no human review step — a materially different risk profile than the current report-and-review model.
- Needs its own staged rollout (feature flag, small account subset first, a full billing cycle of monitoring the new `account_monitor_concurrent_write_prevented_total` metric) before trusting it at full traffic — see Implementation Notes.
- Bundling that rollout into a bug-fix PR would make the PR's blast radius (five unrelated-looking subsystems) even harder to review than it already is.

**Why rejected (for this PR):** the risk/reward of promoting a dormant, live-money-movement code path doesn't belong in the same change as fixing five separate concurrency bugs. This ADR documents the fixed, safety-net state and leaves promotion as an explicit, separately-scoped follow-up.

### Alternative 2: Delete AccountMonitor entirely

**Description:** Since it's never been live, remove `account_monitor.rs` and its tests rather than carrying dormant code with fixed-but-unexercised locking logic.

**Pros:**
- Removes the "two systems for one job" confusion entirely.
- Less surface area to keep in sync with schema/behavior changes to `transactions`.

**Cons:**
- Throws away real, working, well-tested logic (deliberate `BigDecimal` usage, overpayment handling) that closes a real latency gap `ReconciliationJob` doesn't address.
- Forecloses Alternative 1 without the org actually deciding it doesn't want real-time matching.

**Why rejected:** premature — nobody has decided real-time matching isn't wanted, only that it isn't ready to be live unreviewed.

## Implementation Notes

- If Alternative 1 is chosen later: roll out behind a feature flag scoped to a small subset of accounts first (using the tenant-scoped, percentage-aware feature-flag check — see ADR-005 / the `transaction_processor.rs` rollout-percentage fix), monitor `account_monitor_concurrent_write_prevented_total` and Horizon API load for a full billing cycle, then widen.
- `ReconciliationJob`'s new `LeaderElection` gating and the `reconciliation_reports_period_unique` constraint (migration `20260823000001`) should remain regardless of which alternative is eventually chosen — they're correct either way.

## References

- `src/services/reconciliation.rs`, `src/services/account_monitor.rs`
- Migration `20260823000001_reconciliation_reports_unique_period.sql`
- PR that accompanies this ADR (Part C)
