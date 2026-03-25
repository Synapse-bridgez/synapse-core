use bigdecimal::BigDecimal;
use reqwest::StatusCode;
use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use synapse_core::{create_app, AppState};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

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

    let (tx, _rx) = tokio::sync::broadcast::channel(100);
    let query_cache = synapse_core::services::QueryCache::new("redis://localhost:6379").unwrap();

    let app_state = AppState {
        db: pool.clone(),
        pool_manager: synapse_core::db::pool_manager::PoolManager::new(&database_url, None)
            .await
            .unwrap(),
        horizon_client: synapse_core::stellar::HorizonClient::new(
            "https://horizon-testnet.stellar.org".to_string(),
        ),
        feature_flags: synapse_core::services::feature_flags::FeatureFlagService::new(pool.clone()),
        redis_url: "redis://localhost:6379".to_string(),
        start_time: std::time::Instant::now(),
        readiness: synapse_core::ReadinessState::new(),
        tx_broadcast: tx,
        query_cache,
    };
    let app = create_app(app_state);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
    let server = axum::Server::bind(&addr).serve(app.into_make_service());
    let actual_addr = server.local_addr();

    tokio::spawn(async move {
        server.await.unwrap();
    });

    let base_url = format!("http://{}", actual_addr);
    (base_url, pool, container)
}

async fn insert_test_transaction(
    pool: &PgPool,
    stellar_account: &str,
    amount: &str,
    asset_code: &str,
    status: &str,
) -> uuid::Uuid {
    let amount_decimal: BigDecimal = amount.parse().unwrap();
    let id = uuid::Uuid::new_v4();

    sqlx::query(
        r#"
        INSERT INTO transactions (id, stellar_account, amount, asset_code, status, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, NOW(), NOW())
        "#,
    )
    .bind(id)
    .bind(stellar_account)
    .bind(amount_decimal)
    .bind(asset_code)
    .bind(status)
    .execute(pool)
    .await
    .unwrap();

    id
}

#[tokio::test]
async fn test_export_csv_with_filters() {
    let (base_url, pool, _container) = setup_test_app().await;
    let client = reqwest::Client::new();

    insert_test_transaction(&pool, "GABC123", "100.50", "USD", "pending").await;
    insert_test_transaction(&pool, "GDEF456", "200.00", "USD", "completed").await;
    insert_test_transaction(&pool, "GHIJ789", "150.00", "EUR", "pending").await;

    let res = client
        .get(format!("{}/export?format=csv&status=pending", base_url))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers().get("content-type").unwrap(), "text/csv");
    assert!(res
        .headers()
        .get("content-disposition")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("attachment"));

    let body = res.text().await.unwrap();
    assert!(body.contains("id,stellar_account,amount"));
    assert!(body.contains("GABC123"));
    assert!(body.contains("GHIJ789"));
    assert!(!body.contains("GDEF456"));
}

#[tokio::test]
async fn test_export_json_with_filters() {
    let (base_url, pool, _container) = setup_test_app().await;
    let client = reqwest::Client::new();

    insert_test_transaction(&pool, "GABC123", "100.50", "USD", "pending").await;
    insert_test_transaction(&pool, "GDEF456", "200.00", "USDC", "completed").await;

    let res = client
        .get(format!("{}/export?format=json&asset_code=USDC", base_url))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "application/json"
    );

    let body = res.text().await.unwrap();
    assert!(body.contains("GDEF456"));
    assert!(body.contains("USDC"));
    assert!(!body.contains("GABC123"));
}

#[tokio::test]
async fn test_export_date_range() {
    let (base_url, pool, _container) = setup_test_app().await;
    let client = reqwest::Client::new();

    let id1 = uuid::Uuid::new_v4();
    let id2 = uuid::Uuid::new_v4();

    sqlx::query(
        r#"
        INSERT INTO transactions (id, stellar_account, amount, asset_code, status, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $6)
        "#,
    )
    .bind(id1)
    .bind("GABC123")
    .bind(BigDecimal::from(100))
    .bind("USD")
    .bind("pending")
    .bind(chrono::Utc::now() - chrono::Duration::days(5))
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        r#"
        INSERT INTO transactions (id, stellar_account, amount, asset_code, status, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $6)
        "#,
    )
    .bind(id2)
    .bind("GDEF456")
    .bind(BigDecimal::from(200))
    .bind("USD")
    .bind("completed")
    .bind(chrono::Utc::now() - chrono::Duration::days(15))
    .execute(&pool)
    .await
    .unwrap();

    let from_date = (chrono::Utc::now() - chrono::Duration::days(10)).format("%Y-%m-%d");
    let to_date = chrono::Utc::now().format("%Y-%m-%d");

    let res = client
        .get(format!(
            "{}/export?format=csv&from={}&to={}",
            base_url, from_date, to_date
        ))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let body = res.text().await.unwrap();
    assert!(body.contains("GABC123"));
    assert!(!body.contains("GDEF456"));
}

#[tokio::test]
async fn test_export_large_dataset_streaming() {
    let (base_url, pool, _container) = setup_test_app().await;
    let client = reqwest::Client::new();

    for i in 0..2500 {
        insert_test_transaction(
            &pool,
            &format!("GABC{:0>52}", i),
            "100.00",
            "USD",
            "pending",
        )
        .await;
    }

    let res = client
        .get(format!("{}/export?format=csv", base_url))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let body = res.text().await.unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert!(lines.len() > 2500);
}

#[tokio::test]
async fn test_export_empty_results() {
    let (base_url, _pool, _container) = setup_test_app().await;
    let client = reqwest::Client::new();

    let res = client
        .get(format!("{}/export?format=csv&status=nonexistent", base_url))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let body = res.text().await.unwrap();
    assert!(body.contains("id,stellar_account,amount"));
    assert_eq!(body.lines().count(), 1);
}

#[tokio::test]
async fn test_export_headers_and_filename() {
    let (base_url, pool, _container) = setup_test_app().await;
    let client = reqwest::Client::new();

    insert_test_transaction(&pool, "GABC123", "100.50", "USD", "pending").await;

    let res = client
        .get(format!("{}/export?format=csv", base_url))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers().get("content-type").unwrap(), "text/csv");

    let content_disposition = res
        .headers()
        .get("content-disposition")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(content_disposition.starts_with("attachment; filename=\"transactions_"));
    assert!(content_disposition.ends_with(".csv\""));

    let res = client
        .get(format!("{}/export?format=json", base_url))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "application/json"
    );

    let content_disposition = res
        .headers()
        .get("content-disposition")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(content_disposition.starts_with("attachment; filename=\"transactions_"));
    assert!(content_disposition.ends_with(".json\""));
}
