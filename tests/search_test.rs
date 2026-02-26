use chrono::{Duration, Utc};
use reqwest::StatusCode;
use sqlx::types::BigDecimal;
use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use std::str::FromStr;
use synapse_core::db::pool_manager::PoolManager;
use synapse_core::services::feature_flags::FeatureFlagService;
use synapse_core::{create_app, AppState};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use tokio::net::TcpListener;
use uuid::Uuid;

async fn setup_test_app() -> (String, PgPool, impl std::any::Any) {
    let container = Postgres::default().start().await.unwrap();
    let host_port = container.get_host_port_ipv4(5432).await.unwrap();
    let database_url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        host_port
    );

    let pool = PgPool::connect(&database_url).await.unwrap();
    let migrator = Migrator::new(Path::join(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        "migrations",
    ))
    .await
    .unwrap();
    migrator.run(&pool).await.unwrap();

    let pool_manager = PoolManager::new(&database_url, None).await.unwrap();
    let (tx_broadcast, _) = tokio::sync::broadcast::channel(100);

    let app_state = AppState {
        db: pool.clone(),
        pool_manager,
        horizon_client: synapse_core::stellar::HorizonClient::new(
            "https://horizon-testnet.stellar.org".to_string(),
        ),
        feature_flags: FeatureFlagService::new(pool.clone()),
        redis_url: "redis://localhost:6379".to_string(),
        start_time: std::time::Instant::now(),
        readiness: synapse_core::ReadinessState::new(),
        tx_broadcast,
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

    let base_url = format!("http://{}", addr);
    (base_url, pool, container)
}

/// Seed test database with known transactions for predictable assertions
async fn seed_test_data(pool: &PgPool) {
    let now = Utc::now();

    // Transaction 1: USD, pending, recent
    sqlx::query(
        r#"
        INSERT INTO transactions (
            id, stellar_account, amount, asset_code, status,
            created_at, updated_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(Uuid::new_v4())
    .bind("GABC1111111111")
    .bind(BigDecimal::from_str("100").unwrap())
    .bind("USD")
    .bind("pending")
    .bind(now - Duration::hours(1))
    .bind(now - Duration::hours(1))
    .execute(pool)
    .await
    .unwrap();

    // Transaction 2: USD, completed, older
    sqlx::query(
        r#"
        INSERT INTO transactions (
            id, stellar_account, amount, asset_code, status,
            created_at, updated_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(Uuid::new_v4())
    .bind("GDEF2222222222")
    .bind(BigDecimal::from_str("250").unwrap())
    .bind("USD")
    .bind("completed")
    .bind(now - Duration::days(2))
    .bind(now - Duration::days(2))
    .execute(pool)
    .await
    .unwrap();

    // Transaction 3: EUR, completed, recent
    sqlx::query(
        r#"
        INSERT INTO transactions (
            id, stellar_account, amount, asset_code, status,
            created_at, updated_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(Uuid::new_v4())
    .bind("GHIJ3333333333")
    .bind(BigDecimal::from_str("500").unwrap())
    .bind("EUR")
    .bind("completed")
    .bind(now - Duration::hours(2))
    .bind(now - Duration::hours(2))
    .execute(pool)
    .await
    .unwrap();

    // Transaction 4: USD, failed, older
    sqlx::query(
        r#"
        INSERT INTO transactions (
            id, stellar_account, amount, asset_code, status,
            created_at, updated_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(Uuid::new_v4())
    .bind("GKLM4444444444")
    .bind(BigDecimal::from_str("75").unwrap())
    .bind("USD")
    .bind("failed")
    .bind(now - Duration::days(5))
    .bind(now - Duration::days(5))
    .execute(pool)
    .await
    .unwrap();

    // Transaction 5: USDC, completed, mid-range
    sqlx::query(
        r#"
        INSERT INTO transactions (
            id, stellar_account, amount, asset_code, status,
            created_at, updated_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(Uuid::new_v4())
    .bind("GNOP5555555555")
    .bind(BigDecimal::from_str("1000").unwrap())
    .bind("USDC")
    .bind("completed")
    .bind(now - Duration::days(1))
    .bind(now - Duration::days(1))
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_search_by_status() {
    let (base_url, pool, _container) = setup_test_app().await;
    seed_test_data(&pool).await;

    let client = reqwest::Client::new();

    // Search for completed transactions
    let res = client
        .get(format!("{}/transactions/search", base_url))
        .query(&[("status", "completed")])
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let response: serde_json::Value = res.json().await.unwrap();

    assert_eq!(response["total"], 3); // 3 completed transactions
    assert!(response["results"].is_array());

    // Verify all results have completed status
    for tx in response["results"].as_array().unwrap() {
        assert_eq!(tx["status"], "completed");
    }
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_search_by_asset_code() {
    let (base_url, pool, _container) = setup_test_app().await;
    seed_test_data(&pool).await;

    let client = reqwest::Client::new();

    // Search for USD transactions
    let res = client
        .get(format!("{}/transactions/search", base_url))
        .query(&[("asset_code", "USD")])
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let response: serde_json::Value = res.json().await.unwrap();

    assert_eq!(response["total"], 3); // 3 USD transactions

    // Verify all results have USD asset code
    for tx in response["results"].as_array().unwrap() {
        assert_eq!(tx["asset_code"], "USD");
    }
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_search_by_date_range() {
    let (base_url, pool, _container) = setup_test_app().await;
    seed_test_data(&pool).await;

    let client = reqwest::Client::new();
    let now = Utc::now();

    // Search for transactions in the last 3 days
    let from = (now - Duration::days(3)).to_rfc3339();
    let to = now.to_rfc3339();

    let res = client
        .get(format!("{}/transactions/search", base_url))
        .query(&[("from", &from), ("to", &to)])
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let response: serde_json::Value = res.json().await.unwrap();

    // Should return transactions from last 3 days (not the 5-day old one)
    assert_eq!(response["total"], 4);
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_search_pagination() {
    let (base_url, pool, _container) = setup_test_app().await;
    seed_test_data(&pool).await;

    let client = reqwest::Client::new();

    // First page with limit 2
    let res = client
        .get(format!("{}/transactions/search", base_url))
        .query(&[("limit", "2")])
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let page1: serde_json::Value = res.json().await.unwrap();

    assert_eq!(page1["results"].as_array().unwrap().len(), 2);
    assert!(page1["next_cursor"].is_string());

    let cursor = page1["next_cursor"].as_str().unwrap();

    // Second page using cursor
    let res = client
        .get(format!("{}/transactions/search", base_url))
        .query(&[("limit", "2"), ("cursor", cursor)])
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let page2: serde_json::Value = res.json().await.unwrap();

    assert_eq!(page2["results"].as_array().unwrap().len(), 2);

    // Verify no duplicate IDs between pages
    let page1_ids: Vec<&str> = page1["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tx| tx["id"].as_str().unwrap())
        .collect();

    let page2_ids: Vec<&str> = page2["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tx| tx["id"].as_str().unwrap())
        .collect();

    for id in &page1_ids {
        assert!(!page2_ids.contains(id));
    }
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_search_empty_results() {
    let (base_url, pool, _container) = setup_test_app().await;
    seed_test_data(&pool).await;

    let client = reqwest::Client::new();

    // Search for non-existent asset code
    let res = client
        .get(format!("{}/transactions/search", base_url))
        .query(&[("asset_code", "XYZ")])
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let response: serde_json::Value = res.json().await.unwrap();

    assert_eq!(response["total"], 0);
    assert_eq!(response["results"].as_array().unwrap().len(), 0);
    assert!(response["next_cursor"].is_null());
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_search_invalid_parameters() {
    let (base_url, pool, _container) = setup_test_app().await;
    seed_test_data(&pool).await;

    let client = reqwest::Client::new();

    // Invalid date format
    let res = client
        .get(format!("{}/transactions/search", base_url))
        .query(&[("from", "invalid-date")])
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let error: String = res.text().await.unwrap();
    assert!(error.contains("Invalid 'from' date"));

    // Invalid cursor
    let res = client
        .get(format!("{}/transactions/search", base_url))
        .query(&[("cursor", "invalid-cursor")])
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let error: String = res.text().await.unwrap();
    assert!(error.contains("Invalid cursor"));

    // Invalid min_amount
    let res = client
        .get(format!("{}/transactions/search", base_url))
        .query(&[("min_amount", "not-a-number")])
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let error: String = res.text().await.unwrap();
    assert!(error.contains("Invalid 'min_amount'"));
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_search_combined_filters() {
    let (base_url, pool, _container) = setup_test_app().await;
    seed_test_data(&pool).await;

    let client = reqwest::Client::new();

    // Search for completed USD transactions
    let res = client
        .get(format!("{}/transactions/search", base_url))
        .query(&[("status", "completed"), ("asset_code", "USD")])
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let response: serde_json::Value = res.json().await.unwrap();

    // Should return only completed USD transactions
    assert_eq!(response["total"], 1);

    for tx in response["results"].as_array().unwrap() {
        assert_eq!(tx["status"], "completed");
        assert_eq!(tx["asset_code"], "USD");
    }
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_search_by_stellar_account() {
    let (base_url, pool, _container) = setup_test_app().await;
    seed_test_data(&pool).await;

    let client = reqwest::Client::new();

    // Search for specific stellar account
    let res = client
        .get(format!("{}/transactions/search", base_url))
        .query(&[("stellar_account", "GABC1111111111")])
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let response: serde_json::Value = res.json().await.unwrap();

    assert_eq!(response["total"], 1);
    assert_eq!(response["results"][0]["stellar_account"], "GABC1111111111");
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_search_with_amount_range() {
    let (base_url, pool, _container) = setup_test_app().await;
    seed_test_data(&pool).await;

    let client = reqwest::Client::new();

    // Search for transactions between 100 and 500
    let res = client
        .get(format!("{}/transactions/search", base_url))
        .query(&[("min_amount", "100"), ("max_amount", "500")])
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let response: serde_json::Value = res.json().await.unwrap();

    // Should return transactions with amounts 100, 250, and 500
    assert_eq!(response["total"], 3);

    for tx in response["results"].as_array().unwrap() {
        let amount: f64 = tx["amount"].as_str().unwrap().parse().unwrap();
        assert!((100.0..=500.0).contains(&amount));
    }
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_search_limit_boundaries() {
    let (base_url, pool, _container) = setup_test_app().await;
    seed_test_data(&pool).await;

    let client = reqwest::Client::new();

    // Test with limit 1
    let res = client
        .get(format!("{}/transactions/search", base_url))
        .query(&[("limit", "1")])
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let response: serde_json::Value = res.json().await.unwrap();
    assert_eq!(response["results"].as_array().unwrap().len(), 1);
    assert!(response["next_cursor"].is_string());

    // Test with limit exceeding max (should cap at 100)
    let res = client
        .get(format!("{}/transactions/search", base_url))
        .query(&[("limit", "200")])
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let response: serde_json::Value = res.json().await.unwrap();
    // Should return all 5 transactions since we only have 5
    assert_eq!(response["results"].as_array().unwrap().len(), 5);
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_search_no_next_cursor_on_last_page() {
    let (base_url, pool, _container) = setup_test_app().await;
    seed_test_data(&pool).await;

    let client = reqwest::Client::new();

    // Request all results with high limit
    let res = client
        .get(format!("{}/transactions/search", base_url))
        .query(&[("limit", "100")])
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let response: serde_json::Value = res.json().await.unwrap();

    // Should have no next_cursor since all results fit in one page
    assert!(response["next_cursor"].is_null());
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_search_ordering() {
    let (base_url, pool, _container) = setup_test_app().await;
    seed_test_data(&pool).await;

    let client = reqwest::Client::new();

    // Get all transactions
    let res = client
        .get(format!("{}/transactions/search", base_url))
        .query(&[("limit", "100")])
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let response: serde_json::Value = res.json().await.unwrap();
    let results = response["results"].as_array().unwrap();

    // Verify results are ordered by created_at DESC
    for i in 0..results.len() - 1 {
        let current_date = results[i]["created_at"].as_str().unwrap();
        let next_date = results[i + 1]["created_at"].as_str().unwrap();
        assert!(
            current_date >= next_date,
            "Results should be ordered by created_at DESC"
        );
    }
}
