//! Exactly-once webhook delivery verification harness.
//!
//! `migrations/20260601000000_webhook_exactly_once_delivery.sql` and
//! `src/services/webhook_dispatcher.rs` together promise that every logical
//! event - a `(endpoint_id, transaction_id, event_type)` tuple - is
//! delivered to its endpoint exactly once, even when duplicate-triggering
//! conditions pile up (a replica race on the claim, a retry after a
//! transient fault, a crashed worker being reclaimed, a double replay out
//! of the DLQ). That guarantee is easy to silently regress in a future
//! refactor, so this file is a standing harness that re-proves it under
//! concurrent load on every CI run rather than relying on manual review.
//!
//! # Shape
//!
//! Each scenario:
//!   1. stands up a throwaway Postgres (per test) and Redis,
//!   2. points a webhook endpoint at an in-process counting HTTP receiver
//!      whose responses are scripted by a [`ResponsePlan`],
//!   3. applies one duplicate-triggering [`Trigger`],
//!   4. drives `WebhookDispatcher::process_pending` to completion across one
//!      or more dispatcher instances, and
//!   5. asserts the receiver acknowledged the event exactly once and the
//!      `webhook_deliveries` / `webhook_delivery_dlq` rows agree.
//!
//! # Determinism
//!
//! No assertion depends on wall-clock timing. Retry backoff and circuit
//! breaker cooldowns are stepped past by rewriting `next_attempt_at` and
//! deleting the Redis breaker keys directly (see [`fast_forward`]), never by
//! sleeping and hoping. In-flight ordering that a scenario needs is
//! established with an explicit signal from the receiver, not a delay.
//!
//! # Adding a scenario
//!
//! See `docs/webhook-exactly-once-harness.md`. In short: add a [`Trigger`]
//! variant (or reuse one), teach [`Scenario::run`] how to set it up and
//! which assertion to run, then add a `#[tokio::test] #[ignore]` wrapper
//! named `exactly_once_<thing>`. Scenarios that need bespoke orchestration
//! (an in-flight pause, a two-phase setup) are written as standalone tests
//! that reuse the helpers in this file.

use redis::Client as RedisClient;
use sqlx::migrate::Migrator;
use sqlx::PgPool;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use synapse_core::services::WebhookDispatcher;
use testcontainers::{runners::AsyncRunner, ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Mutex, Semaphore};
use uuid::Uuid;

const EVENT_TYPE: &str = "transaction.completed";

// === Infra setup

/// Throwaway Postgres with every migration applied, plus the current-month
/// `transactions` partition (some dispatcher queries touch `transactions`).
async fn setup_postgres() -> (PgPool, ContainerAsync<Postgres>) {
    let container = Postgres::default()
        .with_tag("14-alpine")
        .start()
        .await
        .unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", port);

    let pool = PgPool::connect(&url).await.unwrap();

    Migrator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();

    let _ = sqlx::query(
        r#"
        DO $$
        DECLARE
            p_date DATE := DATE_TRUNC('month', NOW());
            p_name TEXT := 'transactions_y' || TO_CHAR(p_date, 'YYYY') || 'm' || TO_CHAR(p_date, 'MM');
        BEGIN
            IF NOT EXISTS (SELECT 1 FROM pg_class WHERE relname = p_name) THEN
                EXECUTE format(
                    'CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)',
                    p_name,
                    TO_CHAR(p_date, 'YYYY-MM-DD'),
                    TO_CHAR(p_date + INTERVAL '1 month', 'YYYY-MM-DD')
                );
            END IF;
        END $$;
        "#,
    )
    .execute(&pool)
    .await;

    (pool, container)
}

/// Prefer a Redis from `REDIS_URL` (CI supplies one as a service); otherwise
/// start a container. Breaker and rate-limit keys are namespaced by a random
/// endpoint UUID per test, so a shared CI Redis does not cross-contaminate.
async fn setup_redis() -> (String, Option<ContainerAsync<testcontainers::GenericImage>>) {
    if let Ok(url) = std::env::var("REDIS_URL") {
        return (url, None);
    }
    let image = testcontainers::GenericImage::new("redis", "7-alpine");
    let container = image.start().await.unwrap();
    let port = container.get_host_port_ipv4(6379).await.unwrap();
    (format!("redis://127.0.0.1:{}/", port), Some(container))
}

struct Ctx {
    pool: PgPool,
    redis_url: String,
    _pg: ContainerAsync<Postgres>,
    _redis: Option<ContainerAsync<testcontainers::GenericImage>>,
}

async fn setup() -> Ctx {
    let (pool, pg) = setup_postgres().await;
    let (redis_url, redis) = setup_redis().await;
    Ctx {
        pool,
        redis_url,
        _pg: pg,
        _redis: redis,
    }
}

fn redis_client(url: &str) -> RedisClient {
    RedisClient::open(url).unwrap()
}

// === Counting HTTP receiver

/// How the receiver answers each attempt for a given logical event.
#[derive(Clone, Copy)]
enum ResponsePlan {
    /// Always answer `200`.
    AlwaysAccept,
    /// Always answer `500` - used to drive a delivery to exhaustion.
    AlwaysReject,
    /// Close the connection with no response for the first `drops` attempts
    /// (a transport error to the dispatcher), then answer `500` for the next
    /// `errors` attempts, then `200` from then on.
    DropsThenErrorsThenAccept { drops: usize, errors: usize },
    /// Hold every request open until [`Receiver::release_all`], then `200`.
    /// Lets a test pin a delivery in flight while it races a second cycle.
    BlockUntilReleased,
}

struct ReceiverState {
    plan: ResponsePlan,
    // key -> total requests seen
    seen: Mutex<HashMap<String, usize>>,
    // key -> requests answered 2xx
    accepted: Mutex<HashMap<String, usize>>,
    arrivals: broadcast::Sender<String>,
    gate: Semaphore,
}

struct Receiver {
    base_url: String,
    state: Arc<ReceiverState>,
    _accept_loop: tokio::task::JoinHandle<()>,
}

impl Receiver {
    async fn spawn(plan: ResponsePlan) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (arrivals, _) = broadcast::channel(256);
        let state = Arc::new(ReceiverState {
            plan,
            seen: Mutex::new(HashMap::new()),
            accepted: Mutex::new(HashMap::new()),
            arrivals,
            gate: Semaphore::new(0),
        });

        let loop_state = state.clone();
        let accept_loop = tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let conn_state = loop_state.clone();
                tokio::spawn(async move { serve_connection(sock, &conn_state).await });
            }
        });

        Self {
            base_url: format!("http://{addr}"),
            state,
            _accept_loop: accept_loop,
        }
    }

    fn endpoint_url(&self) -> String {
        format!("{}/webhook", self.base_url)
    }

    /// Subscribe before the traffic that should be observed is started.
    fn subscribe(&self) -> broadcast::Receiver<String> {
        self.state.arrivals.subscribe()
    }

    fn release_all(&self) {
        self.state.gate.add_permits(4096);
    }

    async fn seen(&self, key: &str) -> usize {
        *self.state.seen.lock().await.get(key).unwrap_or(&0)
    }

    async fn accepted(&self, key: &str) -> usize {
        *self.state.accepted.lock().await.get(key).unwrap_or(&0)
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn header_value(head: &str, name_lower: &str) -> Option<String> {
    head.lines()
        .find(|line| line.to_ascii_lowercase().starts_with(&format!("{name_lower}:")))
        .and_then(|line| line.split_once(':'))
        .map(|(_, v)| v.trim().to_string())
}

async fn serve_connection(mut sock: TcpStream, state: &ReceiverState) {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];

    // Read until end of headers.
    let header_end = loop {
        let n = match sock.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > 64 * 1024 {
            return;
        }
    };

    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let content_len: usize = header_value(&head, "content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    while buf.len() < header_end + content_len {
        match sock.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }

    let body_end = (header_end + content_len).min(buf.len());
    let body = &buf[header_end..body_end];
    let event_type = header_value(&head, "x-webhook-event").unwrap_or_default();
    let transaction_id = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("transaction_id")
                .and_then(|t| t.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_default();
    let key = format!("{transaction_id}:{event_type}");

    let attempt_ordinal = {
        let mut seen = state.seen.lock().await;
        let count = seen.entry(key.clone()).or_insert(0);
        *count += 1;
        *count
    };
    let _ = state.arrivals.send(key.clone());

    let accept = match state.plan {
        ResponsePlan::AlwaysAccept => true,
        ResponsePlan::AlwaysReject => false,
        ResponsePlan::DropsThenErrorsThenAccept { drops, errors } => {
            if attempt_ordinal <= drops {
                // Drop mid-delivery: no HTTP response at all.
                return;
            }
            attempt_ordinal > drops + errors
        }
        ResponsePlan::BlockUntilReleased => {
            state.gate.acquire().await.unwrap().forget();
            true
        }
    };

    if accept {
        *state.accepted.lock().await.entry(key).or_insert(0) += 1;
        let _ = sock
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
            .await;
    } else {
        let _ = sock
            .write_all(
                b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 3\r\nConnection: close\r\n\r\nERR",
            )
            .await;
    }
    let _ = sock.flush().await;
}

// === Delivery-row helpers

async fn insert_endpoint(pool: &PgPool, url: &str) -> Uuid {
    // max_delivery_rate is set high so the per-endpoint rate limiter never
    // interferes with an exactly-once assertion.
    sqlx::query_scalar(
        r#"
        INSERT INTO webhook_endpoints (url, secret, event_types, max_delivery_rate)
        VALUES ($1, 'harness-secret', ARRAY[$2], 100000)
        RETURNING id
        "#,
    )
    .bind(url)
    .bind(EVENT_TYPE)
    .fetch_one(pool)
    .await
    .expect("insert endpoint")
}

async fn insert_pending_delivery(pool: &PgPool, endpoint_id: Uuid, transaction_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        r#"
        INSERT INTO webhook_deliveries
            (endpoint_id, transaction_id, event_type, payload, status, next_attempt_at)
        VALUES ($1, $2, $3, $4, 'pending', NOW())
        RETURNING id
        "#,
    )
    .bind(endpoint_id)
    .bind(transaction_id)
    .bind(EVENT_TYPE)
    .bind(serde_json::json!({
        "event_type": EVENT_TYPE,
        "transaction_id": transaction_id.to_string(),
        "timestamp": "2026-01-01T00:00:00Z",
        "data": {},
    }))
    .fetch_one(pool)
    .await
    .expect("insert delivery")
}

async fn delivery_ids_for(pool: &PgPool, transaction_id: Uuid) -> Vec<Uuid> {
    sqlx::query_scalar("SELECT id FROM webhook_deliveries WHERE transaction_id = $1")
        .bind(transaction_id)
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn open_delivery_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM webhook_deliveries WHERE status IN ('pending', 'in_progress')",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

// === Driving process_pending deterministically

/// Run one `process_pending` pass on every dispatcher at once (separate
/// tasks, so the claim path is genuinely raced).
async fn run_cycle(dispatchers: &[WebhookDispatcher]) {
    let handles: Vec<_> = dispatchers
        .iter()
        .cloned()
        .map(|d| tokio::spawn(async move { d.process_pending().await }))
        .collect();
    for h in handles {
        h.await
            .expect("process_pending task panicked")
            .expect("process_pending returned an error");
    }
}

/// Step past retry backoff and circuit-breaker cooldowns without sleeping:
/// pull every still-pending delivery's `next_attempt_at` to now and drop the
/// endpoint's Redis breaker / probe / rate keys.
async fn fast_forward(pool: &PgPool, redis: &RedisClient, endpoint_ids: &[Uuid]) {
    sqlx::query("UPDATE webhook_deliveries SET next_attempt_at = NOW() WHERE status = 'pending'")
        .execute(pool)
        .await
        .unwrap();

    let mut conn = redis.get_multiplexed_async_connection().await.unwrap();
    for id in endpoint_ids {
        let _: () = redis::cmd("DEL")
            .arg(format!("webhook_cb:{id}"))
            .arg(format!("webhook_cb_probe:{id}"))
            .arg(format!("webhook_rate:{id}"))
            .query_async(&mut conn)
            .await
            .unwrap();
    }
}

/// Cycle until nothing is pending/in_progress or `max_cycles` is hit.
async fn drive_until_settled(
    dispatchers: &[WebhookDispatcher],
    pool: &PgPool,
    redis: &RedisClient,
    endpoint_ids: &[Uuid],
    max_cycles: usize,
) {
    for cycle in 0..max_cycles {
        run_cycle(dispatchers).await;
        if open_delivery_count(pool).await == 0 {
            return;
        }
        fast_forward(pool, redis, endpoint_ids).await;
        if cycle + 1 == max_cycles {
            eprintln!("drive_until_settled: hit max_cycles={max_cycles} with work still open");
        }
    }
}

// === Exactly-once assertions

/// Count of `attempt_history` entries whose recorded response was a 2xx.
fn successful_attempts(history: &serde_json::Value) -> usize {
    history
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter(|e| {
                    e.get("response_status")
                        .and_then(|s| s.as_i64())
                        .map(|c| (200..300).contains(&c))
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0)
}

async fn assert_delivered_exactly_once(
    receiver: &Receiver,
    key: &str,
    pool: &PgPool,
    delivery_id: Uuid,
) {
    assert_eq!(
        receiver.accepted(key).await,
        1,
        "endpoint must acknowledge exactly one successful delivery for {key}"
    );

    let (status, history): (String, serde_json::Value) = sqlx::query_as(
        "SELECT status, COALESCE(attempt_history, '[]'::jsonb) FROM webhook_deliveries WHERE id = $1",
    )
    .bind(delivery_id)
    .fetch_one(pool)
    .await
    .unwrap();

    assert_eq!(status, "delivered", "delivery row must end 'delivered'");
    assert_eq!(
        successful_attempts(&history),
        1,
        "attempt_history must record exactly one successful attempt"
    );

    let dlq: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM webhook_delivery_dlq WHERE delivery_id = $1")
            .bind(delivery_id)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(dlq, 0, "a delivered event must not also be in the DLQ");
}

async fn assert_exhausted_exactly_once(
    receiver: &Receiver,
    key: &str,
    pool: &PgPool,
    delivery_id: Uuid,
) {
    assert_eq!(
        receiver.accepted(key).await,
        0,
        "an always-failing endpoint must never record a success"
    );

    let (status, attempt_count): (String, i32) =
        sqlx::query_as("SELECT status, attempt_count FROM webhook_deliveries WHERE id = $1")
            .bind(delivery_id)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(status, "failed", "exhausted delivery row must end 'failed'");
    assert_eq!(attempt_count, 5, "exhaustion is exactly MAX_ATTEMPTS attempts");

    let dlq: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM webhook_delivery_dlq WHERE delivery_id = $1")
            .bind(delivery_id)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(dlq, 1, "an exhausted delivery lands in the DLQ exactly once");
}

// === Parameterized scenarios

/// The duplicate-triggering condition a scenario injects.
#[derive(Clone, Copy)]
enum Trigger {
    /// `enqueue()` called `copies` times concurrently for one logical event.
    /// The `ON CONFLICT (endpoint_id, transaction_id, event_type) DO NOTHING`
    /// enqueue must collapse them to a single delivery row.
    DuplicateEnqueue { copies: usize },
    /// `workers` dispatcher replicas call `process_pending` at once against a
    /// single due delivery. `FOR UPDATE SKIP LOCKED` plus the status flip
    /// must let exactly one of them deliver.
    ConcurrentReplicas { workers: usize },
    /// The endpoint drops the connection, then 500s, then recovers. The
    /// retry loop must produce exactly one successful receipt.
    RetryAfterTransientFault { drops: usize, errors: usize },
    /// The endpoint never recovers. The event must exhaust to a single DLQ
    /// row and never be reported delivered.
    ExhaustionToDlq,
    /// A worker claimed the row and then "crashed" (row left `in_progress`
    /// with a stale `claimed_at`). Replicas that reclaim it must still
    /// deliver exactly once.
    ReclaimAfterCrashedClaim { workers: usize },
}

struct Scenario {
    name: &'static str,
    trigger: Trigger,
}

impl Scenario {
    fn response_plan(&self) -> ResponsePlan {
        match self.trigger {
            Trigger::RetryAfterTransientFault { drops, errors } => {
                ResponsePlan::DropsThenErrorsThenAccept { drops, errors }
            }
            Trigger::ExhaustionToDlq => ResponsePlan::AlwaysReject,
            _ => ResponsePlan::AlwaysAccept,
        }
    }

    fn worker_count(&self) -> usize {
        match self.trigger {
            Trigger::ConcurrentReplicas { workers }
            | Trigger::ReclaimAfterCrashedClaim { workers } => workers,
            Trigger::ExhaustionToDlq => 3,
            _ => 1,
        }
    }

    async fn run(&self) {
        eprintln!("exactly-once scenario: {}", self.name);
        let ctx = setup().await;
        let redis = redis_client(&ctx.redis_url);

        let receiver = Receiver::spawn(self.response_plan()).await;
        let endpoint_id = insert_endpoint(&ctx.pool, &receiver.endpoint_url()).await;
        let transaction_id = Uuid::new_v4();
        let key = format!("{transaction_id}:{EVENT_TYPE}");

        let delivery_id = self.arrange(&ctx, endpoint_id, transaction_id).await;

        let dispatchers: Vec<WebhookDispatcher> = (0..self.worker_count())
            .map(|_| WebhookDispatcher::new(ctx.pool.clone(), &ctx.redis_url).unwrap())
            .collect();
        drive_until_settled(&dispatchers, &ctx.pool, &redis, &[endpoint_id], 12).await;

        match self.trigger {
            Trigger::ExhaustionToDlq => {
                assert_exhausted_exactly_once(&receiver, &key, &ctx.pool, delivery_id).await
            }
            _ => assert_delivered_exactly_once(&receiver, &key, &ctx.pool, delivery_id).await,
        }
    }

    /// Create the delivery row in the state the trigger calls for and return
    /// its id.
    async fn arrange(&self, ctx: &Ctx, endpoint_id: Uuid, transaction_id: Uuid) -> Uuid {
        match self.trigger {
            Trigger::DuplicateEnqueue { copies } => {
                let dispatcher =
                    WebhookDispatcher::new(ctx.pool.clone(), &ctx.redis_url).unwrap();
                let handles: Vec<_> = (0..copies)
                    .map(|_| {
                        let d = dispatcher.clone();
                        tokio::spawn(async move {
                            d.enqueue(transaction_id, EVENT_TYPE, serde_json::json!({"k": "v"}))
                                .await
                        })
                    })
                    .collect();
                for h in handles {
                    h.await.unwrap().expect("enqueue");
                }

                let ids = delivery_ids_for(&ctx.pool, transaction_id).await;
                assert_eq!(
                    ids.len(),
                    1,
                    "concurrent duplicate enqueue must create exactly one delivery row"
                );
                ids[0]
            }
            Trigger::ReclaimAfterCrashedClaim { .. } => {
                let id = insert_pending_delivery(&ctx.pool, endpoint_id, transaction_id).await;
                sqlx::query(
                    "UPDATE webhook_deliveries \
                     SET status = 'in_progress', claimed_at = NOW() - INTERVAL '10 minutes' \
                     WHERE id = $1",
                )
                .bind(id)
                .execute(&ctx.pool)
                .await
                .unwrap();
                id
            }
            _ => insert_pending_delivery(&ctx.pool, endpoint_id, transaction_id).await,
        }
    }
}

// === Scenario tests

#[tokio::test]
#[ignore = "Requires Docker (testcontainers) and Redis"]
async fn exactly_once_duplicate_enqueue() {
    Scenario {
        name: "duplicate-enqueue",
        trigger: Trigger::DuplicateEnqueue { copies: 4 },
    }
    .run()
    .await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers) and Redis"]
async fn exactly_once_concurrent_replicas() {
    Scenario {
        name: "concurrent-replicas",
        trigger: Trigger::ConcurrentReplicas { workers: 4 },
    }
    .run()
    .await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers) and Redis"]
async fn exactly_once_retry_after_transient_fault() {
    Scenario {
        name: "retry-after-transient-fault",
        trigger: Trigger::RetryAfterTransientFault { drops: 1, errors: 1 },
    }
    .run()
    .await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers) and Redis"]
async fn exactly_once_exhaustion_routes_to_single_dlq_entry() {
    Scenario {
        name: "exhaustion-to-dlq",
        trigger: Trigger::ExhaustionToDlq,
    }
    .run()
    .await;
}

#[tokio::test]
#[ignore = "Requires Docker (testcontainers) and Redis"]
async fn exactly_once_reclaim_after_crashed_claim() {
    Scenario {
        name: "reclaim-after-crashed-claim",
        trigger: Trigger::ReclaimAfterCrashedClaim { workers: 2 },
    }
    .run()
    .await;
}

// === Bespoke scenarios
//
// These need orchestration the parameterized runner does not model: pausing
// a delivery mid-flight, or a two-phase setup. They reuse the same helpers
// and the same exactly-once assertions.

/// A second `process_pending` cycle that overlaps an in-flight delivery must
/// not start a duplicate attempt. The first attempt is pinned open at the
/// receiver; while it is parked, a second dispatcher runs a full cycle and
/// must find nothing to do (the row is `in_progress` with a fresh
/// `claimed_at`, so neither the pending branch nor the reclaim branch of the
/// claim query matches it).
#[tokio::test]
#[ignore = "Requires Docker (testcontainers) and Redis"]
async fn exactly_once_inflight_delivery_not_redelivered_by_overlapping_cycle() {
    let ctx = setup().await;
    let receiver = Receiver::spawn(ResponsePlan::BlockUntilReleased).await;
    let endpoint_id = insert_endpoint(&ctx.pool, &receiver.endpoint_url()).await;
    let transaction_id = Uuid::new_v4();
    let key = format!("{transaction_id}:{EVENT_TYPE}");
    let delivery_id = insert_pending_delivery(&ctx.pool, endpoint_id, transaction_id).await;

    let mut arrivals = receiver.subscribe();

    let d1 = WebhookDispatcher::new(ctx.pool.clone(), &ctx.redis_url).unwrap();
    let inflight = tokio::spawn(async move { d1.process_pending().await });

    // The claim commits well before the HTTP request leaves, so once the
    // receiver reports the arrival the row is definitely `in_progress`.
    let arrived = tokio::time::timeout(Duration::from_secs(30), arrivals.recv())
        .await
        .expect("the in-flight attempt should reach the receiver")
        .unwrap();
    assert_eq!(arrived, key);

    let d2 = WebhookDispatcher::new(ctx.pool.clone(), &ctx.redis_url).unwrap();
    d2.process_pending().await.unwrap();
    assert_eq!(
        receiver.seen(&key).await,
        1,
        "overlapping cycle must not start a second delivery of an in-flight event"
    );

    receiver.release_all();
    inflight.await.unwrap().unwrap();

    assert_delivered_exactly_once(&receiver, &key, &ctx.pool, delivery_id).await;
}

/// Replaying the same DLQ entry twice - concurrently - must converge on a
/// single live delivery row (`ON CONFLICT ... DO UPDATE`) and therefore a
/// single successful receipt, not two.
#[tokio::test]
#[ignore = "Requires Docker (testcontainers) and Redis"]
async fn exactly_once_double_replay_from_dlq_delivers_once() {
    let ctx = setup().await;
    let redis = redis_client(&ctx.redis_url);

    // Phase 1: exhaust an event into the DLQ.
    let rejecting = Receiver::spawn(ResponsePlan::AlwaysReject).await;
    let endpoint_id = insert_endpoint(&ctx.pool, &rejecting.endpoint_url()).await;
    let transaction_id = Uuid::new_v4();
    let key = format!("{transaction_id}:{EVENT_TYPE}");
    let delivery_id = insert_pending_delivery(&ctx.pool, endpoint_id, transaction_id).await;

    let dispatcher = WebhookDispatcher::new(ctx.pool.clone(), &ctx.redis_url).unwrap();
    drive_until_settled(
        std::slice::from_ref(&dispatcher),
        &ctx.pool,
        &redis,
        &[endpoint_id],
        12,
    )
    .await;

    let dlq_id: Uuid =
        sqlx::query_scalar("SELECT id FROM webhook_delivery_dlq WHERE delivery_id = $1")
            .bind(delivery_id)
            .fetch_one(&ctx.pool)
            .await
            .expect("phase 1 should have produced a DLQ row");

    // Phase 2: point the endpoint at an accepting receiver, then replay the
    // DLQ entry twice at once.
    let accepting = Receiver::spawn(ResponsePlan::AlwaysAccept).await;
    sqlx::query("UPDATE webhook_endpoints SET url = $1 WHERE id = $2")
        .bind(accepting.endpoint_url())
        .bind(endpoint_id)
        .execute(&ctx.pool)
        .await
        .unwrap();
    fast_forward(&ctx.pool, &redis, &[endpoint_id]).await;

    let (r1, r2) = tokio::join!(
        dispatcher.replay_from_dlq(dlq_id),
        dispatcher.replay_from_dlq(dlq_id)
    );
    let live_id = r1.expect("replay 1");
    assert_eq!(
        live_id,
        r2.expect("replay 2"),
        "concurrent replays must converge on one live delivery row"
    );

    drive_until_settled(
        std::slice::from_ref(&dispatcher),
        &ctx.pool,
        &redis,
        &[endpoint_id],
        12,
    )
    .await;

    assert_eq!(
        accepting.accepted(&key).await,
        1,
        "a double replay must still deliver the event exactly once"
    );
    let status: String =
        sqlx::query_scalar("SELECT status FROM webhook_deliveries WHERE id = $1")
            .bind(live_id)
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
    assert_eq!(status, "delivered");

    let dlq_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM webhook_delivery_dlq WHERE delivery_id = $1")
            .bind(delivery_id)
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
    assert_eq!(dlq_rows, 1, "replay must not add a second DLQ row");
}
