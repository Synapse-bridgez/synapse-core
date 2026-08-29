# Async Bulk Status Jobs

## Summary

`PATCH /admin/transactions/bulk-status` previously processed every batch
synchronously, regardless of size. Under high transaction volume, large batches
exceeded reasonable HTTP timeouts and gave operators no way to retrieve partial
results after a mid-flight failure.

This PR adds threshold-based routing: small batches keep the existing
synchronous behaviour (backward-compatible), while large batches are queued as
background jobs and immediately return `202 Accepted` with a `job_id`.

---

## Changes

### `migrations/20260827000000_bulk_status_jobs.sql`
New `bulk_status_jobs` table tracking job state (`pending → running →
completed | failed`), the full `transaction_ids[]`, per-item `result_summary`
JSONB, and timing columns (`started_at`, `completed_at`).

### `src/handlers/admin/bulk_status.rs`
- **Threshold routing** — configurable via `BULK_STATUS_ASYNC_THRESHOLD` env var
  (default 50). Batches at or below the threshold use the existing sync path;
  batches above it are enqueued.
- **`enqueue_job`** — inserts a `pending` row into `bulk_status_jobs` and returns
  the UUID.
- **`run_job`** — tokio background task that processes the batch in 200-item
  chunks through the same `bulk_update_transaction_status` query the sync path
  uses, so per-tenant quota limits remain in effect. Marks the job `running` on
  start, `completed` with a per-item JSONB summary on success, or `failed` with
  an `error_message` on any chunk error.
- **`GET /admin/transactions/bulk-status/jobs/:id`** — new polling endpoint
  returning job state, counts, result summary, and timing.
- Hard cap raised from 500 → 10,000 items (large batches go async anyway).
- Existing unit tests preserved; four new unit tests cover validation
  edge-cases and the sync/async threshold boundary.

### `src/lib.rs`
Registers the new `GET /admin/transactions/bulk-status/jobs/:id` route under
the existing `admin_only_routes` block (same auth gate as the PATCH route).

---

## API

### Sync response (batch ≤ threshold)
```
HTTP 200 OK
{
  "updated": 12,
  "failed": 0,
  "errors": []
}
```

### Async response (batch > threshold)
```
HTTP 202 Accepted
{
  "job_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "pending",
  "message": "Batch of 1500 transactions queued as async job. Poll GET /admin/transactions/bulk-status/jobs/550e8400-... for results."
}
```

### Poll job status
```
GET /admin/transactions/bulk-status/jobs/:id

HTTP 200 OK
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "completed",          // pending | running | completed | failed
  "transaction_count": 1500,
  "target_status": "failed",
  "result_summary": {
    "updated": 1498,
    "failed": 2,
    "errors": [
      { "transaction_id": "...", "error": "invalid status transition" }
    ]
  },
  "error_message": null,
  "created_at": "2026-08-27T19:30:00Z",
  "started_at": "2026-08-27T19:30:01Z",
  "completed_at": "2026-08-27T19:30:04Z"
}
```

---

## Out of scope (per issue)
- Generalising this into a framework for other admin endpoints
- Changes to quota middleware or retry backoff

---

## Testing
Unit tests in `bulk_status.rs` cover:
- Request deserialisation
- Empty `transaction_ids` rejected
- Invalid `status` value rejected
- Exceeding 10,000-item cap rejected
- Valid request accepted
- Threshold boundary: exactly-at-threshold → sync path
- Threshold boundary: one-over-threshold → async path

Integration tests (DB required) are left for follow-up in
`tests/load/` per the issue's acceptance criteria for load testing.
