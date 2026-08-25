use reqwest::StatusCode;
use serde_json::json;
use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use synapse_core::db::pool_manager::PoolManager;
use synapse_core::services::feature_flags::FeatureFlagService;
use synapse_core::{create_app, AppState};
use tokio::net::TcpListener;

/// Connects to `DATABASE_URL`, runs migrations, ensures the current-month
/// transactions partition exists, spawns a live `create_app` server on an
/// ephemeral port, and returns `(reqwest client, graphql_url, callback_url,
/// pool)` — the shared setup every test in this file needs to exercise the
/// real `/graphql` HTTP route end to end. Returns `None` if `DATABASE_URL`
/// isn't set, so callers can skip cleanly.
/// `POST /callback` now requires a valid HMAC signature (see
/// `middleware::webhook_signature::verify_anchor_signature`) — the app built
/// here has no `SecretsStore`, so the middleware falls back to this env var.
const TEST_WEBHOOK_SECRET: &str = "graphql-test-webhook-secret";

/// Signs `body` the same way `cache::webhook::verify_signature` expects:
/// HMAC-SHA256 over `{timestamp}.{body}`. Returns `(timestamp, signature)`.
fn sign_webhook_body(body: &[u8]) -> (String, String) {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string();

    let mut mac = Hmac::<Sha256>::new_from_slice(TEST_WEBHOOK_SECRET.as_bytes()).unwrap();
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));

    (timestamp, signature)
}

async fn spawn_test_app() -> Option<(reqwest::Client, String, String, PgPool)> {
    std::env::set_var("ANCHOR_WEBHOOK_SECRET", TEST_WEBHOOK_SECRET);
    let database_url = std::env::var("DATABASE_URL").ok()?;

    let pool = PgPool::connect(&database_url).await.unwrap();
    let migrator = Migrator::new(Path::join(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        "migrations",
    ))
    .await
    .unwrap();
    migrator.run(&pool).await.unwrap();

    // Create partition for current month
    let _ = sqlx::query(
        r#"
        DO $$
        DECLARE
            partition_date DATE;
            partition_name TEXT;
            start_date TEXT;
            end_date TEXT;
        BEGIN
            partition_date := DATE_TRUNC('month', NOW());
            partition_name := 'transactions_y' || TO_CHAR(partition_date, 'YYYY') || 'm' || TO_CHAR(partition_date, 'MM');
            start_date := TO_CHAR(partition_date, 'YYYY-MM-DD');
            end_date := TO_CHAR(partition_date + INTERVAL '1 month', 'YYYY-MM-DD');

            IF NOT EXISTS (SELECT 1 FROM pg_class WHERE relname = partition_name) THEN
                EXECUTE format(
                    'CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)',
                    partition_name, start_date, end_date
                );
            END IF;
        END $$;
        "#
    )
    .execute(&pool)
    .await;

    let pool_manager = PoolManager::new(&database_url, None, 5).await.unwrap();
    let feature_flags = FeatureFlagService::new(pool.clone());
    let (tx_broadcast, _) = tokio::sync::broadcast::channel(100);
    let readiness = synapse_core::ReadinessState::new();

    let app_state = AppState {
        db: pool.clone(),
        pool_manager,
        horizon_client: synapse_core::stellar::HorizonClient::new(
            "https://horizon-testnet.stellar.org".to_string(),
        ),
        feature_flags,
        redis_url: "redis://localhost:6379".to_string(),
        start_time: std::time::Instant::now(),
        tx_broadcast,
        readiness,
        query_cache: synapse_core::services::QueryCache::new("redis://localhost:6379")
            .await
            .unwrap(),
        allowed_ips: synapse_core::config::AllowedIps::Any,
        trusted_proxy_depth: 1,
        profiling_manager: synapse_core::handlers::profiling::ProfilingManager::new(),
        tenant_configs: std::sync::Arc::new(tokio::sync::RwLock::new(
            std::collections::HashMap::new(),
        )),
        pending_queue_depth: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        current_batch_size: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(10)),
        secrets_store: None,
        metrics_handle: synapse_core::metrics::init_metrics().unwrap(),
        ws_connection_pool: std::sync::Arc::new(
            synapse_core::ws::connection_pool::ConnectionPool::new(
                synapse_core::ws::connection_pool::PoolConfig::default(),
            ),
        ),
    };
    let app = create_app(app_state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::Server::from_tcp(listener.into_std().unwrap())
            .unwrap()
            .serve(app.into_make_service())
            .await
            .unwrap();
    });

    let client = reqwest::Client::new();
    Some((
        client,
        format!("http://{}/graphql", addr),
        format!("http://{}/callback", addr),
        pool,
    ))
}

#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_graphql_queries() {
    let Some((client, graphql_url, callback_url, _pool)) = spawn_test_app().await else {
        println!("Skipping GraphQL test: DATABASE_URL not set");
        return;
    };

    let query = json!({
        "query": "{ transactions { id status } }"
    });
    let res = client
        .post(&graphql_url)
        .header("Authorization", "Bearer admin-secret-key")
        .json(&query)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let payload = json!({
        "stellar_account": "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "amount": "100.50",
        "asset_code": "USD",
        "callback_type": "deposit",
        "callback_status": "completed"
    });
    let body_bytes = serde_json::to_vec(&payload).unwrap();
    let (ts, sig) = sign_webhook_body(&body_bytes);
    let res = client
        .post(&callback_url)
        .header("X-Webhook-Timestamp", ts)
        .header("X-Webhook-Signature", sig)
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let tx: serde_json::Value = res.json().await.unwrap();
    let tx_id = tx["id"].as_str().unwrap();

    let query = json!({
        "query": format!("{{ transaction(id: \"{}\") {{ id status amount assetCode }} }}", tx_id)
    });
    let res = client
        .post(&graphql_url)
        .header("Authorization", "Bearer admin-secret-key")
        .json(&query)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["data"]["transaction"]["id"], tx_id);

    // BigDecimal may have trailing zeros, so parse and compare numerically
    let amount_str = body["data"]["transaction"]["amount"].as_str().unwrap();
    let amount: f64 = amount_str.parse().unwrap();
    assert_eq!(amount, 100.50);

    assert_eq!(body["data"]["transaction"]["assetCode"], "USD");
}

// ---------------------------------------------------------------------------
// GraphQL complexity scoring, exercised through the live HTTP `/graphql`
// route (Part C regression coverage). Until Part C's fix,
// `handlers::graphql::graphql_handler` was a hand-rolled string-matching
// stand-in that never called into the real async-graphql schema at all — so
// this test previously had to call `AppSchema::execute` directly to exercise
// anything, which proved the resolver-level `#[graphql(complexity = ...)]`
// annotations worked in isolation but not that they were actually reachable
// from production traffic. It now goes through `create_app` + a real HTTP
// POST to `/graphql`, so a regression back to a non-executing stand-in
// handler would fail this test.
// ---------------------------------------------------------------------------

#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_graphql_complexity_scales_with_requested_limit() {
    let Some((client, graphql_url, _callback_url, _pool)) = spawn_test_app().await else {
        println!("Skipping GraphQL complexity test: DATABASE_URL not set");
        return;
    };

    // Exactly at the alias cap (20), each requesting the max page size (1000).
    // Under the old field-occurrence-only accounting this stayed well under
    // MAX_QUERY_COMPLEXITY (1000) despite up to 20,000 row-equivalent work;
    // with limit-scaled complexity it must now be rejected.
    let aliased_fields: String = (0..20)
        .map(|i| format!("a{i}: transactions(limit: 1000) {{ id }}"))
        .collect::<Vec<_>>()
        .join(" ");
    let exploit_query = json!({ "query": format!("{{ {aliased_fields} }}") });

    let res = client
        .post(&graphql_url)
        .header("Authorization", "Bearer admin-secret-key")
        .json(&exploit_query)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK); // GraphQL errors are 200 + an errors[] array, not an HTTP error
    let body: serde_json::Value = res.json().await.unwrap();
    let errors = body["errors"]
        .as_array()
        .expect("expected an errors array rejecting the over-complex query");
    assert!(
        !errors.is_empty(),
        "20 aliases x limit:1000 should be rejected by the complexity limit, got: {:?}",
        body
    );
    assert!(
        errors.iter().any(|e| e["message"]
            .as_str()
            .unwrap_or_default()
            .to_lowercase()
            .contains("complex")),
        "expected a complexity-limit error, got: {:?}",
        errors
    );

    // A single modest-limit query must still pass through the same live
    // route, proving this isn't a blanket rejection.
    let modest_query = json!({ "query": "{ transactions(limit: 20) { id } }" });
    let res = client
        .post(&graphql_url)
        .header("Authorization", "Bearer admin-secret-key")
        .json(&modest_query)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(
        body.get("errors").is_none() || body["errors"].as_array().unwrap().is_empty(),
        "modest query should not trip the complexity limit: {:?}",
        body
    );
}

// ---------------------------------------------------------------------------
// Part D regression tests: forceCompleteTransaction must validate the
// current status and be CAS-guarded, not unconditionally overwrite whatever
// state the transaction was in.
// ---------------------------------------------------------------------------

#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_force_complete_transaction_rejects_invalid_state() {
    let Some((client, graphql_url, _callback_url, pool)) = spawn_test_app().await else {
        println!("Skipping test: DATABASE_URL not set");
        return;
    };

    let tx_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO transactions (id, stellar_account, amount, asset_code, status, created_at, updated_at) \
         VALUES (gen_random_uuid(), 'GFAILEDTEST00000000000000000000000000000000000000000', 10.00, 'USD', 'failed', NOW(), NOW()) \
         RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    let mutation = json!({
        "query": format!(
            "mutation {{ forceCompleteTransaction(id: \"{}\") {{ id status }} }}",
            tx_id
        )
    });
    let res = client
        .post(&graphql_url)
        .header("Authorization", "Bearer admin-secret-key")
        .json(&mutation)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(
        body.get("errors").is_some() && !body["errors"].as_array().unwrap().is_empty(),
        "forceCompleteTransaction on an already-failed transaction should be rejected, got: {:?}",
        body
    );

    let status: String = sqlx::query_scalar("SELECT status FROM transactions WHERE id = $1")
        .bind(tx_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "failed", "status must not have changed");
}

#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_force_complete_transaction_concurrent_calls_only_one_succeeds() {
    let Some((client, graphql_url, _callback_url, pool)) = spawn_test_app().await else {
        println!("Skipping test: DATABASE_URL not set");
        return;
    };

    let tx_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO transactions (id, stellar_account, amount, asset_code, status, created_at, updated_at) \
         VALUES (gen_random_uuid(), 'GCONCURRENTTEST0000000000000000000000000000000000000', 10.00, 'USD', 'pending', NOW(), NOW()) \
         RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    let mutation = json!({
        "query": format!(
            "mutation {{ forceCompleteTransaction(id: \"{}\") {{ id status }} }}",
            tx_id
        )
    });

    let (res_a, res_b) = tokio::join!(
        client
            .post(&graphql_url)
            .header("Authorization", "Bearer admin-secret-key")
            .json(&mutation)
            .send(),
        client
            .post(&graphql_url)
            .header("Authorization", "Bearer admin-secret-key")
            .json(&mutation)
            .send()
    );

    let body_a: serde_json::Value = res_a.unwrap().json().await.unwrap();
    let body_b: serde_json::Value = res_b.unwrap().json().await.unwrap();

    let a_ok = body_a.get("errors").is_none() || body_a["errors"].as_array().unwrap().is_empty();
    let b_ok = body_b.get("errors").is_none() || body_b["errors"].as_array().unwrap().is_empty();

    assert_eq!(
        [a_ok, b_ok].iter().filter(|ok| **ok).count(),
        1,
        "expected exactly one concurrent forceCompleteTransaction call to succeed, got \
         a_ok={a_ok} b_ok={b_ok} (a={body_a:?} b={body_b:?})",
    );

    let status: String = sqlx::query_scalar("SELECT status FROM transactions WHERE id = $1")
        .bind(tx_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "completed");
}
