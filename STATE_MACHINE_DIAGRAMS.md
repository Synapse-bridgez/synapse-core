# Unified State Machine Diagrams

## Transaction Status State Machine

```
┌─────────────────────────────────────────────────────────────────┐
│                    TRANSACTION STATES                            │
└─────────────────────────────────────────────────────────────────┘

                              dlq
                               │
                               ├──────────────┐
                               │              │
                               v              │
                            pending ─────────┴─→ processing
                              │ │                    │
                              │ │                    ├─→ completed
                              │ │                    │
                              │ │                    └─→ failed
                              │ │
                              │ │  (reprocess)
                              │ └──────────────────────→ pending
                              │
                              └─→ completed
                              
                              └─→ failed

Valid Transitions (7 total):
├─ pending → processing       ✓
├─ pending → completed        ✓
├─ pending → failed           ✓
├─ processing → completed     ✓
├─ processing → failed        ✓
├─ failed → pending           ✓ (reprocess)
└─ dlq → pending              ✓ (requeue)

Same-state transitions: Always valid (idempotent)
└─ X → X for any state      ✓
```

## Settlement Status State Machine

```
┌─────────────────────────────────────────────────────────────────┐
│                   SETTLEMENT STATES                              │
└─────────────────────────────────────────────────────────────────┘

           voided ◄───────────────┐
             │                    │
             │                    │
completed ───┤                    ├─→ pending_review
             │                    │
             │                    ├─→ disputed ────┬─→ adjusted
             │                    │                │
             └────────────────────┴────────────────┴─→ completed

Valid Transitions (7 total):
├─ completed → pending_review    ✓
├─ pending_review → disputed     ✓
├─ pending_review → voided       ✓
├─ pending_review → completed    ✓ (cancel review)
├─ disputed → adjusted           ✓
├─ disputed → voided             ✓
└─ adjusted → completed          ✓

Same-state transitions: Always valid (idempotent)
└─ X → X for any state          ✓
```

## Transition Paths

### Settlement Dispute Path
```
completed ─→ pending_review ─→ disputed ─→ adjusted ─→ completed
  (settle)    (to review)    (disputed)   (adjusted)   (final)
```

### Settlement Void Path
```
completed ─→ pending_review ─→ voided
  (settle)    (to review)     (released)
```

### Settlement Cancel Review
```
completed ─→ pending_review ─→ completed
  (settle)    (to review)     (cancel)
```

### Settlement Adjustment Path (Full)
```
completed ─→ pending_review ─→ disputed ─→ adjusted ─→ completed
  (settle)    (to review)    (disputed)   (adjust $)   (final)
```

### Transaction Normal Flow
```
pending ─→ processing ─→ completed
 (new)    (processing)   (confirmed)
```

### Transaction Failure Path
```
pending ─→ processing ─→ failed ─→ pending
 (new)    (processing)  (failed)  (reprocess)
```

### Transaction DLQ Recovery
```
dlq ─→ pending ─→ processing ─→ completed
 (x)  (requeue)  (processing)  (confirmed)
```

## TOCTOU Race Fix Diagram

### Before (Vulnerable)

```
Time ─────────────────────────────────────────────────────────────>

Task A                          Task B
──────────────────────────────────────────────────────────────────
READ status='pending_review'
  (unlocked ─ RACE WINDOW)    READ status='pending_review'
                                (unlocked)
Validate:                       Validate:
  pending_review→disputed✓        pending_review→voided✓

LOCK row                        (waiting for lock)
UPDATE status=disputed
  WHERE id=123                  LOCK row (now)
COMMIT ✓                        UPDATE status=voided
                                  WHERE id=123 (no guard!)
Result: status='voided'         COMMIT ✓
  (Should be 'disputed'!)
  ❌ WRONG! Voided→Disputed invalid!
```

### After (Safe)

```
Time ─────────────────────────────────────────────────────────────>

Task A                          Task B
──────────────────────────────────────────────────────────────────
READ status='pending_review'
  (unlocked ─ advisory)       READ status='pending_review'
                                (unlocked)
Validate:                       Validate:
  pending_review→disputed✓        pending_review→voided✓

LOCK row                        (waiting for lock)
Re-validate in lock:            (waiting for lock)
  current.status='pending_review'
  expected='pending_review'
  ✓ Match
UPDATE status=disputed
  WHERE id=123 AND status='pending_review'
  → 1 row affected ✓           (waiting for lock)
COMMIT ✓

Result: status='disputed'       LOCK row (now)
  ✓ CORRECT!                   Re-validate in lock:
                                  current.status='disputed'
                                  expected='pending_review'
                                  ✗ Mismatch!
                               Return Err(RowNotFound)
                               → Err(StaleTransition) ✓
                               ROLLBACK

Result: Task B gets 409 Conflict
  ✓ CORRECT! Exactly one succeeds
```

## Implementation Mapping

| State Machine | Defined in | Consumed by | Validator |
|---|---|---|---|
| **Transaction** | `state_transitions.rs` | `validation/state_machine.rs` | `validate_status_transition()` |
| **Settlement** | `state_transitions.rs` | `services/settlement.rs` | `is_valid_transition()` |
| **Query** | `state_transitions.rs` | `db/queries.rs` | re-validation in lock |

## Error Codes

| Scenario | Error | HTTP Status | Code |
|----------|-------|-------------|------|
| Invalid transition | `InvalidStatusTransition` | 400 | TRANSACTION_005 |
| Concurrent settlement mod | `StaleTransition` | 409 | **SETTLEMENT_003** |
| Settlement not found | `NotFound` | 404 | NOT_FOUND_001 |

## Summary Statistics

| Metric | Count |
|--------|-------|
| Transaction transitions | 7 |
| Settlement transitions | 7 |
| Total unique (from, to) pairs | 14 |
| Same-state transitions allowed | ∞ (all states) |
| TOCTOU race conditions fixed | 1 (settlement updates) |
| Duplicate rule definitions eliminated | 2 |
| New error variants | 1 |
| Test cases added | 10+ |
