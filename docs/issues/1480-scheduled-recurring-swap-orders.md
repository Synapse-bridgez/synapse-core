# Issue #1480 — Scheduled and Recurring Swap Orders with Execution Windows

## Problem

Phase 2 swap orders currently have no future-dated or recurring entry point.
Operators and tenants need to:

- specify a future `execute_after` time plus an optional recurrence rule
  (`interval`, `count`, optional `end date`),
- have a scheduler worker lease and materialize due occurrences through the
  standard order-placement path inside a configurable execution window,
- pause / resume / cancel a parent schedule, and inspect occurrences and the
  parent schedule independently,
- recover from downtime with an explicit catch-up policy (`execute_late` or
  `skip`), with all times in UTC and idempotency keys preventing duplicate
  occurrences.

## Blocking discovery

`src/swap` is intentionally **not wired into `src/lib.rs`**, and the Phase 2
order-placement path it would call (`SwapQuote` / `SwapOrder` models, order
ingestion, `/swap/orders`) does not exist in this Phase 1 repo. Implementing a
scheduler worker that "materializes due occurrences via the standard
order-placement path" would therefore invent a large part of Phase 2 that is
outside this issue. The feature should land against the real Phase 2 swap
service, not this Phase 1 hand-off boundary.

## Approach

Keep the schedule domain in the swap service boundary. Add a durable schedule
model and a lease-based worker that is deliberately independent from the
in-memory cron `JobScheduler` (which runs Phase 1 maintenance jobs).

### Proposed model (PostgreSQL)

```sql
CREATE TYPE swap_schedule_status AS ENUM ('active', 'paused', 'cancelled', 'completed');

CREATE TABLE swap_schedules (
    id                  UUID PRIMARY KEY,
    tenant_id           UUID,                    -- nullable: platform scope
    execute_after       TIMESTAMPTZ NOT NULL,
    recurrence_interval INTERVAL,                -- null = one-shot
    recurrence_count    INTEGER,                 -- total occurrences incl. first
    recurrence_end      TIMESTAMPTZ,
    execution_window    INTERVAL NOT NULL DEFAULT '30 seconds',
    catch_up_policy     TEXT NOT NULL DEFAULT 'execute_late', -- 'execute_late' | 'skip'
    status              swap_schedule_status NOT NULL DEFAULT 'active',
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE swap_schedule_occurrences (
    id             UUID PRIMARY KEY,
    schedule_id    UUID NOT NULL REFERENCES swap_schedules(id),
    occurrence_at  TIMESTAMPTZ NOT NULL,
    window_start   TIMESTAMPTZ NOT NULL,
    window_end     TIMESTAMPTZ NOT NULL,
    idempotency_key TEXT NOT NULL UNIQUE,
    status         TEXT NOT NULL DEFAULT 'pending', -- pending | materialized | skipped
    materialized_order_id UUID,
    lease_owner    TEXT,
    lease_expires_at TIMESTAMPTZ,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX swap_schedules_status_due_idx
    ON swap_schedules (status, execute_after)
    WHERE status IN ('active', 'paused');

CREATE INDEX swap_schedule_occurrences_due_idx
    ON swap_schedule_occurrences (status, occurrence_at)
    WHERE status = 'pending';
```

### Proposed module layout

- `src/swap/schedule.rs` — `SwapSchedule`, `RecurrenceRule`, `CatchUpPolicy`,
  `ScheduleStatus`, `OccurrenceStatus`, UTC occurrence calculation and
  idempotency-key derivation (`swap-schedule:{schedule_id}:{occurrence_index}`).
- `src/swap/schedule_repository.rs` — SQLx queries for create, pause, resume,
  cancel, and lease-acquire (`UPDATE ... WHERE lease_expires_at IS NULL OR
  lease_expires_at < now()` returning rows, guarded by `FOR UPDATE SKIP LOCKED`).
- `src/swap/schedule_worker.rs` — periodic poller that selects due pending
  occurrences, leases each one, and calls the existing order-placement service
  with the occurrence idempotency key. Lease TTL must exceed the longest
  order-placement call so two workers never materialize the same occurrence.
- `src/handlers/swap_schedules.rs` — `POST /swap/schedules`, `POST
  /swap/schedules/{id}/pause|resume|cancel`, `GET /swap/schedules/{id}`,
  `GET /swap/schedules/{id}/occurrences`.
- `src/config.rs` — worker poll interval, lease TTL, default execution window,
  and catch-up policy.

## Steps (in order)

1. Add the Phase 2 `SwapOrder` / `SwapQuote` models and order-placement service
   that the scheduler will call (out of scope for this Phase 1 repo).
2. Write and review the migration from the SQL above; run
   `./scripts/check-migration-safety.sh`.
3. Implement `src/swap/schedule.rs` recurrence math with UTC-only tests for
   due/not-due boundaries and window edges.
4. Implement `src/swap/schedule_repository.rs` with lease-based dedupe so two
   workers cannot materialize the same occurrence.
5. Implement `src/swap/schedule_worker.rs`; on `skip` catch-up mark missed
   occurrences `skipped` without placing an order; on `execute_late` place them
   immediately with their original idempotency key.
6. Implement pause / resume / cancel handlers and wire routes.
7. Add scheduler tests covering due/not-due boundaries, two-worker lease dedupe,
   catch-up after simulated downtime, and pause / resume / cancel.

## Done when

- A schedule with `execute_after` in the future and a recurrence rule creates
  one materialized order per occurrence within the configured window.
- Two scheduler workers never create duplicate occurrences for the same lease.
- Pausing stops future materialization, resuming restarts it, cancelling stops
  permanently, and all three are inspectable via API.
- Simulated downtime produces either `execute_late` or `skip` behavior exactly
  once per occurrence, and every timestamp stored or compared is UTC.
