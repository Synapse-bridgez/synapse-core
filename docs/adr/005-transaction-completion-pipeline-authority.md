# ADR-005: Transaction Completion Pipeline Authority — processor.rs vs. transaction_processor.rs

## Status

Proposed

## Context

This codebase has two independently-built implementations of "move a transaction from `pending` to `completed`":

1. **`processor.rs::process_batch`** — the live pipeline. It's invoked from `ProcessorPool::start()`, which `main.rs` actually spawns. It correctly claims rows with `SELECT ... FOR UPDATE SKIP LOCKED`. Until this PR, the claimed rows were never written back to (`for _transaction in pending { /* TODO: per-transaction processing logic */ }`) — the live pipeline claimed transactions safely and then did nothing with them. Part E of this PR replaces that stub with real completion logic (status write + outbound webhook enqueue) inside the same locked transaction.
2. **`transaction_processor.rs::TransactionProcessor`** — a full staged pipeline (validate → enrich → verify → complete), feature-flag-gated per stage, with a `with_webhook_dispatcher` hook. It is unreachable from any live route or job (its only real caller, `dlq.rs`, only invokes `requeue_dlq`, not `process_transaction`). `lock_examples.rs`, which appeared in earlier code-review notes as a caller, isn't even part of the compiled crate — there is no `mod lock_examples;` anywhere, and it wouldn't compile if added (it imports a type `mod.rs` doesn't re-export).

Both had real bugs, both now fixed in this PR: `CompleteStage`'s completion write had no row lock or status guard (now uses `SELECT ... FOR UPDATE` + `WHERE status = $2`); its `EnrichStage`/`VerifyStage` gating used a feature-flag check that silently ignored `rollout_percentage` (now uses the percentage-aware, account-scoped check). But fixing those bugs in isolation doesn't answer which pipeline the org actually wants running transactions through — and having two, only one of which is exercised in production, is exactly the kind of split that lets a bug in the unreachable one sit invisible for years.

## Decision

**`processor.rs::process_batch` is the authoritative, live transaction-completion pipeline.** This PR completes its previously-stubbed completion logic (row-locked status write + webhook enqueue, see Part E) rather than routing production traffic through `TransactionProcessor`.

`TransactionProcessor` remains in the tree, now with both bugs fixed as a safety net, but is **not** wired into any live route or job by this PR. Its staged, feature-flag-gated design (enrich/verify as optional stages, webhook-dispatcher hook) is a reasonable shape for future work — e.g. if per-transaction enrichment or verification steps are needed — but promoting it to replace `process_batch` is a separate, deliberate migration, not a side effect of this bug-fix PR.

## Consequences

### Positive

- Exactly one code path completes transactions in production, so there's one place to look when auditing or debugging completion behavior.
- `process_batch`'s existing `FOR UPDATE SKIP LOCKED` claim already covers the new completion write with no additional locking needed — the safe pattern was already in place, it just wasn't finishing the job.
- `TransactionProcessor`'s now-fixed bugs mean it's no longer a landmine if someone does wire it in later without re-auditing it.

### Negative

- `transaction_processor.rs` and `lock_examples.rs` remain as substantial, tested-but-unreachable code (roughly 250 lines) that must still be kept compiling and up to date with schema changes, for a pipeline nothing calls. `lock_examples.rs` in particular is already broken (references an unexported type) and stays that way since it isn't part of the build.
- The staged enrich/verify concept isn't available in the live pipeline. If per-transaction enrichment or verification is needed, it will need to be added to `process_batch` directly or `TransactionProcessor` will need to be promoted later.

### Neutral

- This doesn't delete the unreachable pipeline, only formally declares it non-authoritative — deletion is offered as an explicit future option below, not decided here.

## Alternatives Considered

### Alternative 1: Promote TransactionProcessor to live, retire process_batch's inline logic

**Description:** Wire `TransactionProcessor::process_transaction` into the scheduled job / route instead of extending `process_batch`.

**Pros:** Gets the staged, feature-flag-gated enrich/verify architecture and the webhook-dispatcher hook "for free" — this is likely closer to what `transaction_processor.rs` was originally built for.

**Cons:** `process_batch` already safely claims rows in batches; `TransactionProcessor::process_transaction` operates one transaction at a time with its own `pool.begin()` per stage rather than one batch-claim transaction, so switching would change the batching/locking model, not just swap an implementation. That's a bigger, riskier change to bundle into a bug-fix PR.

**Why rejected (for this PR):** `process_batch`'s stub was the more surgical fix — it already had the hard part (safe row claiming) done; it just needed the completion write filled in.

### Alternative 2: Delete transaction_processor.rs and lock_examples.rs entirely

**Description:** Since neither is live, and `lock_examples.rs` doesn't even compile as part of the crate, remove both rather than carrying dead code with newly-fixed-but-unexercised bugs.

**Pros:** Removes ~250 lines of code that must be maintained (kept compiling, kept in sync with schema) for zero production behavior. Removes the "which one is real" confusion for the next person who greps for transaction completion logic.

**Cons:** Throws away a reasonable staged-pipeline design that might be exactly what's needed if per-transaction enrich/verify steps become a real requirement later.

**Why rejected (for now):** same reasoning as ADR-004's Alternative 2 — no one has decided the staged-pipeline concept isn't wanted, only that it isn't ready to replace `process_batch` today. Leaving it fixed-but-dormant costs maintenance overhead but preserves the option; deleting forecloses it. If this ADR is revisited and the answer is still "no," deletion should happen then, not be deferred indefinitely.

## Implementation Notes

- `process_batch`'s completion write (Part E of the accompanying PR) happens inside the same `pool.begin()` / `FOR UPDATE SKIP LOCKED` transaction that claims the batch, so no new locking primitive was needed there.
- If `lock_examples.rs` is kept, it should at minimum be fixed to compile (`mod lock_examples;` declared, correct import of whatever lock type it's demonstrating) or removed — leaving uncompiled example code with a misleading name in `src/services/` is worse than deleting it. Not addressed in this PR (out of scope for the concurrency-bug fixes) — filed here for whoever next touches this file.

## References

- `src/services/processor.rs`, `src/services/transaction_processor.rs`, `src/services/lock_examples.rs`
- [ADR-004: Payment-Matching Authority](004-payment-matching-authority.md) — the analogous decision for reconciliation
- PR that accompanies this ADR (Parts D and E)
