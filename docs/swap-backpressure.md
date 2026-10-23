# Swap admission and overload

One shared `AdmissionController` bounds swap handler work. Route layers assign
`Mutation`, `Quote`, or `Analytics`; clients cannot choose their priority.
Mutation requests have reserved concurrency and queued mutations run before quotes,
which run before analytics. Already admitted work is never preempted. Reserved
slots deliberately remain unused by reads, so quote floods cannot occupy all order
capacity. Orders can still receive 503 when their own bounded queue is full or
their wait deadline expires; this is an explicit retry response before execution,
not a dropped in-flight order. Retry order placement with its idempotency key.
The controller never cancels an admitted handler. If the server cancels a handler
because the client disconnects or shutdown interrupts it, its admission permit is
released. Order persistence must use atomic transactions and durable idempotency;
an admission permit does not make an HTTP future durable.

Every queue is finite. A full queue, queue timeout, or adaptive rejection uses
`SwapApiError::Unavailable`, yielding RFC 7807 HTTP 503 and `Retry-After`.
Clients should respect that delay and add jitter. No request is reclassified or
transparently replayed. Cancellation removes queued entries and releases grants,
including cancellation after a grant but before the caller polls it.

Read traffic is shed deterministically when total utilization reaches the
configured fraction, or handler latency EWMA exceeds the target. Completion samples
use weight 0.2; stale samples expire after five idle seconds, permitting recovery.
Order work bypasses adaptive shedding. This is admission control; existing circuit
breakers continue to control upstream health independently.

## Configuration

Invalid settings fail startup. All durations are milliseconds; booleans are
`true`/`false`. Set `SWAP_BACKPRESSURE_ENABLED=false` to bypass admission for incident
response. Restart to apply settings. Zero queue depth permits immediate admission
but disables waiting for that tier. Reserved capacity must be below total capacity.

| Environment variable | Default |
| --- | --- |
| `SWAP_BACKPRESSURE_ENABLED` | `true` |
| `SWAP_BACKPRESSURE_MAX_IN_FLIGHT` | `64` |
| `SWAP_BACKPRESSURE_MUTATION_RESERVED` | `16` |
| `SWAP_BACKPRESSURE_MUTATION_QUEUE_DEPTH` | `128` |
| `SWAP_BACKPRESSURE_QUOTE_QUEUE_DEPTH` | `64` |
| `SWAP_BACKPRESSURE_ANALYTICS_QUEUE_DEPTH` | `16` |
| `SWAP_BACKPRESSURE_MUTATION_WAIT_MS` | `2000` |
| `SWAP_BACKPRESSURE_QUOTE_WAIT_MS` | `250` |
| `SWAP_BACKPRESSURE_ANALYTICS_WAIT_MS` | `50` |
| `SWAP_BACKPRESSURE_SHED_SATURATION` | `0.9` |
| `SWAP_BACKPRESSURE_LATENCY_TARGET_MS` | `500` |

## Wiring and monitoring

Construct one controller with
`AdmissionController::new(AdmissionConfig::from_env()?)?`, then clone its `Arc` into
`AdmissionRoute { controller, priority }` and apply
`axum::middleware::from_fn_with_state(route, admission_middleware)` to each swap
route. Deposit callback routes use `Priority::Mutation`, with HMAC authentication
running before mutation admission. Future order handlers must also use that tier;
quote REST, WS and SSE handshakes use `Priority::Quote`; reporting routes use
`Priority::Analytics`. Quote admission runs before the tenant extractor, bounding
authentication database work as well as handshake work. An overloaded quote route
can therefore return 503 before authentication is checked. Stream handshakes release their permit
when the streaming response starts; connection/subscription/buffer limits separately
bound stream lifetime. A long-lived stream never pins a mutation slot.

The default Prometheus registry exposes `swap_admission_in_flight{tier}`,
`swap_admission_queue_depth{tier}`, `swap_admission_rejected_total{tier,reason}`
(`adaptive`, `queue_full`, `timeout`), and `swap_admission_saturation`. Keep a single
controller per process, with shared route state. Scrape `/metrics` and load
`monitoring/swap-backpressure-alerts.yml` into Prometheus. Watch mutation rejection
first, then utilization and read shedding. Increase concurrency only after checking
database and upstream headroom; queue depth alone cannot fix sustained overload.

Unit tests cover reserved capacity, priority dispatch, finite queues, deadlines,
queued/granted cancellation, adaptive recovery and disablement. The burst test
submits 256 requests and checks bounded completion with empty queues afterward.
Tests were authored without execution as requested; run them in CI before merge.
