# PR: Unified Settlement Transition State Machine & TOCTOU Fix

**Branch**: `feature/settlement-transition-toctou`

## Summary

Eliminates duplicate hardcoded transition tables and fixes a time-of-check/time-of-use (TOCTOU) race condition in settlement status updates. Both transaction and settlement domains now consume a single declarative state machine definition. Settlement updates are now atomic and self-consistent with re-validation inside the lock and a conditional UPDATE guard.

## Problem

### Issue 1: Duplicated Transition Tables
- `src/validation/state_machine.rs::validate_status_transition` (transaction rules)
- `src/services/settlement.rs::valid_transition` (settlement rules)
- Separate implementations → risk of silent drift
- No single source of truth

### Issue 2: TOCTOU Race in Settlement Updates
```
READ current status (unlocked)
  ↓ (can race here - another task changes status)
VALIDATE transition
  ↓
LOCK row
  ↓
UPDATE unconditionally (WHERE id = $x, no status guard)
```
Result: Two concurrent tasks can both validate, then one clobbers the other's state.

**Example**: Both reviewers validate pending_review→disputed and pending_review→voided, then serially apply both updates. The second overwrites the first, creating an invalid disputed→voided transition.

## Solution

### Unified State Machine (new file)

**`src/validation/state_transitions.rs`**
```rust
pub const TRANSACTION_TRANSITIONS: &[Transition] = &[
    Transition { from: "pending", to: "processing" },
    Transition { from: "pending", to: "completed" },
    // ... 5 more
];

pub const SETTLEMENT_TRANSITIONS: &[Transition] = &[
    Transition { from: "completed", to: "pending_review" },
    Transition { from: "pending_review", to: "disputed" },
    // ... 5 more
];

pub fn is_valid_transition(from: &str, to: &str, allowed: &[Transition]) -> bool {
    if from == to { return true; }  // idempotent
    allowed.iter().any(|t| t.from == from && t.to == to)
}
```

### Atomic Settlement Updates

**Before (vulnerable)**:
```
Lock row
UPDATE settlements SET status = $1 WHERE id = $6  // No status guard!
```

**After (safe)**:
```
Lock row (FOR UPDATE)
Re-validate: if current.status != expected_from_status { Err(StaleTransition) }
UPDATE settlements SET ... WHERE id = $6 AND status = $7  // Status guard!
```

If another task changed the status after the pre-lock read:
- Re-validation inside lock catches it
- UPDATE with status guard affects 0 rows
- Returns `RowNotFound` → mapped to `StaleTransition` (409 Conflict)

## Changes

### Created Files
- ✅ `src/validation/state_transitions.rs` – Unified transition definitions
- ✅ `tests/settlement_toctou_race_test.rs` – Comprehensive state machine tests
- ✅ `docs/settlement-transition-unification.md` – Architecture + diagrams
- ✅ `IMPLEMENTATION_SUMMARY.md` – Detailed change documentation
- ✅ `CHECKLIST.md` – Requirements verification

### Modified Files

| File | Changes |
|------|---------|
| `src/validation/mod.rs` | Export `state_transitions` module |
| `src/validation/state_machine.rs` | Use unified definition; remove duplicate logic |
| `src/services/settlement.rs` | Use unified definition; pass `expected_from_status` to queries |
| `src/db/queries.rs` | **Atomic update**: re-validate in lock + status guard + conflict detection |
| `src/error.rs` | Add `StaleTransition` error (409 Conflict) + error code ERR_SETTLEMENT_003 |

## Testing

### Unit Tests (10+ tests)
```bash
cargo test settlement_toctou_race_test
```
- Verifies unified transitions match original behavior (both domains)
- Tests idempotent same-state transitions
- Confirms no duplicate transitions
- Validates error types

### Integration Tests (requires database)
```bash
DATABASE_URL=postgres://... cargo test --test settlement_dispute_test
```
- Concurrent update scenarios
- Audit log consistency
- Settlement void/dispute/adjustment paths

## Backward Compatibility

✅ **No breaking changes**
- Public APIs unchanged: `validate_status_transition()`, `update_status()`
- All valid/invalid transitions preserved exactly
- Audit logging unchanged
- All existing tests pass without modification

⚠️ **New behavior** (clients should handle)
- Settlement updates can now return 409 Conflict with error code `ERR_SETTLEMENT_003`
- Clients should retry on `StaleTransition` with exponential backoff

## State Machines

### Transaction Status
```
dlq ──→ pending ──→ processing ──→ completed
         ↑_____________↑________________↓
                       │              failed
                       └────────────────┘
```

### Settlement Status
```
completed ──→ pending_review ──┬──→ disputed ──→ adjusted ──→ completed
              ↑_________________│_________________________________↓
              └────────voided────┘
```

## Acceptance Criteria Met

✅ One declarative source of truth (consumed by both domains)
✅ Concurrent conflicting transitions: exactly one wins, other gets `StaleTransition` (409)
✅ All pre-existing valid/invalid transitions preserved
✅ Audit rows still written on success
✅ Transition validation now atomic and self-consistent

## Code Review Notes

- Minimal implementation: only changes necessary for correctness
- TOCTOU fix uses standard database locking patterns (FOR UPDATE + WHERE guard)
- Error handling is deterministic and testable
- No changes to business logic, only safety improvements
