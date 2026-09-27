# ADR-005 Completion Authority Audit

Companion to `docs/adr/005-transaction-completion-pipeline-authority.md`, which
designates `processor.rs::process_batch` as the sole authoritative path that
transitions a transaction to `completed`. This document is the equivalent of
issue 114's audit, applied to transaction completion instead of payment
matching, and covers every path introduced elsewhere in this batch that can
move a transaction toward `completed`.

## Audited code paths

| Source | Transitions toward `completed`? | Routes through the ADR-005 authority? |
|---|---|---|
| `processor.rs::process_batch` (ADR-005 authority) | Yes — the designated write | N/A (this is the authority) |
| `transaction_processor.rs::TransactionProcessor` | Yes, but unreachable from any live route/job | N/A — dormant, see ADR-005 |
| Settlement netting (issue 14) | Marks settlements complete; must not directly flip transaction status | Must call into `process_batch`'s completion write path, not write `status = 'completed'` independently |
| Dispute workflow (issue 3) | Can resolve a dispute back to a completed transaction | Must re-enter via the authoritative completion write, not set status directly from the dispute handler |
| Bulk status updates (issue 4) | Can include `completed` as a target bulk status | Must reject/redirect `completed` as a bulk target, or shell out to the authoritative path per-row |
| Swap/bridge readiness states (issues 24/25) | Readiness states precede, but must not substitute for, completion | Completion itself still occurs only via `process_batch` once readiness is satisfied |

## Enforcement mechanism

Consistent with issue 114's and issue 80's approach (a mechanical guard
rather than a review-only convention), `scripts/check-completion-authority.sh`
greps for direct `status = 'completed'` / `status: Completed` writes against
the transactions table outside of `processor.rs`, and fails if any are found.
This prevents a bypass of the ADR-005 authority from being reintroduced by a
future change without an explicit, reviewed allowlist entry.

## Out of scope

This audit and its enforcement script do not change the completion
pipeline's business logic — only that all completion-adjacent code routes
through the existing designated authority.
