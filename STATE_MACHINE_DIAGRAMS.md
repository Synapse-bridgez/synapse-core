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

## Swap Order Status State Machine

Phase 2 swap orders (`swap_orders` table, `SwapOrderStatus` in `db/models.rs`).
Orders start at `swap_ready` when a funded deposit is ready to be turned into an
executable order.

```
┌─────────────────────────────────────────────────────────────────┐
│                    SWAP ORDER STATES                             │
└─────────────────────────────────────────────────────────────────┘

swap_ready ──→ quoting ──→ quoted ──→ executing ──→ settled
    │              │          │            │
    │              │          │            └─→ failed
    │              │          │
    ├──────────────┴──────────┴─→ cancelled
    │              │          │
    │              └──────────┴─→ failed
```

Valid Transitions (8 total):
├─ swap_ready → quoting       ✓
├─ swap_ready → cancelled     ✓
├─ quoting → quoted           ✓
├─ quoting → failed           ✓
├─ quoting → cancelled        ✓
├─ quoted → executing         ✓
├─ quoted → failed            ✓
├─ quoted → cancelled         ✓
├─ executing → settled        ✓
└─ executing → failed         ✓

Terminal states: `settled`, `failed`, `cancelled` (no outgoing edges).
Once `executing`, an order can no longer be cancelled.

Same-state transitions: Always valid (idempotent, no write)
└─ X → X for any state      ✓

Concurrency: each transition is a compare-and-set on `(id, created_at, version)`.
A stale read returns `SwapOrderTransitionError::Conflict` and the caller re-fetches.

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
| **Swap order** | `db/models.rs` (`SwapOrderStatus`) | `db/queries.rs` (`transition_swap_order`) | `can_transition_to()`, version CAS |

## Error Codes

| Scenario | Error | HTTP Status | Code |
|----------|-------|-------------|------|
| Invalid transition | `InvalidStatusTransition` | 400 | TRANSACTION_005 |
| Concurrent settlement mod | `StaleTransition` | 409 | **SETTLEMENT_003** |
| Settlement not found | `NotFound` | 404 | NOT_FOUND_001 |

## Swap Order State Machine

Phase 2 (`src/swap/order.rs`). Pure domain logic: `SwapState::transition_to`
enforces the table below, and `SwapOrder` applies it with caller-supplied UTC
timestamps and actor metadata.

```
swap_ready → quote_requested → quoted → order_created → executing → executed → settlement_pending → settled

Off-path exits:
  quoted → expired                  (quote TTL elapsed)
  any non-terminal state → failed
  swap_ready / quote_requested / quoted / order_created → cancelled

Terminal states: settled, failed, cancelled, expired (no outgoing edges)
```

Valid transitions (`SwapState::allowed_next`):

| From | To |
|------|----|
| `swap_ready` | `quote_requested`, `cancelled`, `failed` |
| `quote_requested` | `quoted`, `cancelled`, `failed` |
| `quoted` | `order_created`, `expired`, `cancelled`, `failed` |
| `order_created` | `executing`, `cancelled`, `failed` |
| `executing` | `executed`, `failed` |
| `executed` | `settlement_pending`, `failed` |
| `settlement_pending` | `settled`, `failed` |

Guards:

- Self-transitions (`X → X`) are rejected with `SelfTransition`. Unlike the
  transaction states above, swap states are not idempotent.
- `quoted` is entered only via `SwapOrder::record_quote`, which rejects a quote
  for another pair or one already expired.
- `order_created` requires an unexpired quote at the transition instant.
  An expired quote is rejected with `QuoteExpired` and the state is unchanged;
  the order may only move forward to `expired`, never back to `quoted`.
- Stable string names (`as_str`, serde, `FromStr`) match the snake_case names above.

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

## Swap Order Execution (#1436)

Status is governed by `SWAP_ORDER_TRANSITIONS` in `src/swap/order.rs`; every change goes
through `execution::transition_order` (guard + conditional `WHERE status = from AND lease_owner = me`).

```
quoted ──► executing ──► filled
   │          ├──► partially_filled ──► filled
   │          └──► reconciling  (venue timeout / unknown outcome)
   │                 └──► filled / partially_filled
   └──► failed  (also from executing / partially_filled / reconciling:
                 permanent venue error, retries exhausted)
```

Flow per attempt (events in `swap_order_events`: started, submitted, partially_filled, filled, failed):

1. Worker leases orders with `FOR UPDATE SKIP LOCKED`, setting `lease_owner`/`lease_expires_at`
   (visibility timeout; crashed workers' orders are reclaimed once the lease expires).
2. `quoted → executing`, then `ExecutionVenue::submit` with idempotency key `swap-order:<order_id>`,
   so a crash mid-submission re-submits the same key and the venue dedupes.
3. Fills are inserted into `swap_fills` (unique on `order_id, venue_fill_id`, so replays are no-ops)
   and the total is reconciled from the table; total >= amount → `filled`, else `partially_filled`.
4. `Transient` → lease released with exponential backoff, up to `max_attempts`, then `failed`
   (`RETRIES_EXHAUSTED`). `Permanent` → `failed` (`VENUE_REJECTED`).
5. `Unknown` (timeout) → `reconciling`; later attempts only call `ExecutionVenue::lookup`, never
   `submit`. Exhaustion → `failed` (`RECONCILIATION_FAILED`).

The worker is started in `main.rs` only when `SWAP_ENGINE_ENABLED=true`.

## Swap Order Settlement

```
executed ──► settlement_pending ──► settled
                  │
                  └──► failed (reason code, on definitive rejection)
```

- `executed → settlement_pending`: an atomic claim increments `settlement_attempt`; idempotency key is `swap-settle:{order_id}:{attempt}`.
- The payout tx hash is persisted in `swap_orders.settlement_tx_hash` **before** submission, and each attempt is logged in `swap_events`.
- Horizon timeouts/5xx are *unknown*, not failed: submission is retried (same signed envelope, same hash) with exponential backoff, then left `settlement_pending`.
- If a hash already exists, the order is only reconciled (Horizon `GET /transactions/{hash}`), never re-submitted.
- `settlement_pending → settled`: on confirmation; a `swap_fills` row records the hash. The raw external status is stored in `settlement_external_status`.
- `settlement_pending → failed`: on definitive rejection, with `failure_reason_code`.
- The reconciliation loop (started in `main.rs`) re-checks orders pending longer than `SETTLEMENT_STUCK_THRESHOLD_SECS` (default 300) every `SETTLEMENT_RECONCILE_INTERVAL_SECS` (default 60) and emits a `swap_settlement_stuck` error-level alert if still unresolved. Also configurable: `SETTLEMENT_MAX_SUBMIT_ATTEMPTS`, `SETTLEMENT_MAX_BACKOFF_SECS`. Horizon endpoint: `STELLAR_HORIZON_URL`.
