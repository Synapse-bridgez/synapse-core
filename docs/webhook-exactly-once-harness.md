# Webhook Exactly-Once Delivery Harness

`tests/webhook_concurrent_delivery_test.rs` is a standing harness that
re-proves, on every CI run, that the outbound webhook subsystem delivers
each logical event exactly once under duplicate-triggering load.

- **Guarantee under test:** for every logical event - a
  `(endpoint_id, transaction_id, event_type)` tuple - the receiving endpoint
  acknowledges the event with a `2xx` exactly once, and the
  `webhook_deliveries` / `webhook_delivery_dlq` rows agree with that.
- **Code it guards:**
  `migrations/20260601000000_webhook_exactly_once_delivery.sql` and
  `src/services/webhook_dispatcher.rs` (`enqueue`, `process_pending`,
  `handle_failure`, `route_to_dlq`, `replay_from_dlq`).
- **Why it exists:** exactly-once is a strong claim that a future refactor of
  the dispatcher can silently break. Manual review does not reliably catch
  that; a red CI check does.

## How it works

Each scenario:

1. stands up a throwaway Postgres (one container per test) and a Redis
   (`REDIS_URL` in CI, otherwise a container);
2. points a `webhook_endpoints` row at an in-process **counting HTTP
   receiver** whose per-attempt responses are scripted by a `ResponsePlan`
   (`AlwaysAccept`, `AlwaysReject`, `DropsThenErrorsThenAccept`,
   `BlockUntilReleased`);
3. applies one duplicate-triggering `Trigger`;
4. drives `WebhookDispatcher::process_pending` to completion across one or
   more dispatcher instances (`drive_until_settled`);
5. asserts exactly-once via `assert_delivered_exactly_once` or
   `assert_exhausted_exactly_once`.

The receiver records, per logical event, both the total number of requests
seen and the number it answered `2xx`. Exactly-once means the `2xx` count is
`1` (or `0` with one DLQ row, for the exhaustion scenario).

## Determinism

The harness must not be timing-flaky in CI, so:

- **Retry backoff and circuit-breaker cooldowns are stepped past, never
  waited out.** `fast_forward` pulls every still-pending
  `next_attempt_at` to `NOW()` and deletes the endpoint's Redis
  `webhook_cb:*` / `webhook_cb_probe:*` / `webhook_rate:*` keys between
  cycles.
- **In-flight ordering uses an explicit signal.** The `BlockUntilReleased`
  receiver parks a request open and notifies the test through a broadcast
  channel; the test reacts to that signal rather than sleeping.
- **The claim race is real, not simulated.** `run_cycle` spawns each
  dispatcher's `process_pending` on its own task; correctness comes from
  `FOR UPDATE SKIP LOCKED` plus the status flip, which the database
  serializes regardless of task scheduling.
- `tokio::time::timeout` appears only as an upper-bound safety net on a
  signal that resolves in milliseconds on the happy path.

## Scenarios

| Test | Trigger | What it proves |
|---|---|---|
| `exactly_once_duplicate_enqueue` | 4 concurrent `enqueue()` for one event | `ON CONFLICT (endpoint_id, transaction_id, event_type) DO NOTHING` collapses to one delivery row |
| `exactly_once_concurrent_replicas` | 4 dispatchers race one due delivery | `FOR UPDATE SKIP LOCKED` + status flip lets exactly one replica deliver |
| `exactly_once_retry_after_transient_fault` | connection drop, then `500`, then recovery | the retry loop yields exactly one successful receipt and one success in `attempt_history` |
| `exactly_once_exhaustion_routes_to_single_dlq_entry` | endpoint never recovers | event exhausts to a single `webhook_delivery_dlq` row, never reported delivered |
| `exactly_once_reclaim_after_crashed_claim` | row left `in_progress` with a stale `claimed_at`, 2 replicas reclaim it | reclaim of a crashed worker still delivers exactly once |
| `exactly_once_inflight_delivery_not_redelivered_by_overlapping_cycle` | second `process_pending` cycle overlaps an in-flight attempt | an in-flight (`in_progress`, fresh `claimed_at`) row is invisible to both the pending and the reclaim branch of the claim query |
| `exactly_once_double_replay_from_dlq_delivers_once` | same DLQ entry replayed twice concurrently | `replay_from_dlq`'s `ON CONFLICT ... DO UPDATE` converges on one live row and one receipt |

## Running it

```
# whole harness (needs Docker + a local Redis, or REDIS_URL set)
cargo test --test webhook_concurrent_delivery_test -- --ignored --nocapture

# one scenario
cargo test --test webhook_concurrent_delivery_test exactly_once_concurrent_replicas -- --ignored
```

In CI it runs as the **"Run webhook exactly-once delivery harness"** step of
the `integration-tests` job in `.github/workflows/rust.yml`. The broad
`-- --ignored` step in the same job skips `exactly_once_` so the harness is
not run twice; the `coverage` job still exercises it.

## Adding a scenario

For a scenario that fits the common shape (set up one delivery, drive it,
assert exactly-once):

1. If an existing `Trigger` variant covers the condition, reuse it.
   Otherwise add a variant describing the duplicate-triggering condition and
   a short comment on which dispatcher mechanism absorbs it.
2. If the new variant needs a non-default response behavior, extend
   `Scenario::response_plan`; if it needs more than one dispatcher, extend
   `Scenario::worker_count`.
3. If it needs the delivery row to start in a special state, extend
   `Scenario::arrange`; otherwise it gets a plain pending row.
4. Add a `#[tokio::test]` wrapper named `exactly_once_<thing>` with
   `#[ignore = "Requires Docker (testcontainers) and Redis"]` that builds the
   `Scenario` and calls `.run().await`.

For a scenario that needs bespoke orchestration - pausing a delivery
mid-flight, a two-phase setup, driving `replay_from_dlq` directly - write a
standalone `#[tokio::test]` in the `Bespoke scenarios` section that reuses
`setup`, `Receiver`, `insert_endpoint`, `insert_pending_delivery`,
`drive_until_settled`, and the two `assert_*_exactly_once` helpers.

Keep every new assertion independent of wall-clock timing. If a scenario
needs to observe an intermediate state, drive it there with SQL or a
receiver signal - do not sleep.

New `exactly_once_*` tests are picked up by CI automatically: the dedicated
step runs the whole test binary, and
`scripts/check-test-file-ci-coverage.sh` already treats the file as covered.

## Out of scope

Extending exactly-once guarantees to GraphQL subscriptions - that is a
separate delivery path with its own tracking.
