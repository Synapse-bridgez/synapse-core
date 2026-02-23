use synapse_core::{create_app, AppState};
use testcontainers_modules::postgres::Postgres;
use testcontainers::runners::AsyncRunner;
use sqlx::{PgPool, migrate::Migrator};
use std::path::Path;
use tokio::net::TcpListener;
use reqwest::StatusCode;

#[tokio::test]
async fn test_api_versioning_headers() {
    let container = Postgres::default().start().await.unwrap();
    let host_port = container.get_host_port_ipv4(5432).await.unwrap();
    let database_url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", host_port);

    // Run migrations
    let pool = PgPool::connect(&database_url).await.unwrap();
    let migrator = Migrator::new(Path::join(Path::new(env!("CARGO_MANIFEST_DIR")), "migrations")).await.unwrap();
    migrator.run(&pool).await.unwrap();

    // Start App
    let app_state = AppState {
        db: pool.clone(),
        horizon_client: synapse_core::stellar::HorizonClient::new("https://horizon-testnet.stellar.org".to_string()),
    };
    let app = create_app(app_state);
    
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let base_url = format!("http://{}", addr);

    // 1. Test V1 health (expect deprecation headers)
    let res = client.get(&format!("{}/v1/health", base_url))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers().contains_key("Deprecation"));
    assert!(res.headers().contains_key("Sunset"));
    assert_eq!(res.headers().get("Deprecation").unwrap(), "true");

    // 2. Test V2 health (no deprecation headers)
    let res = client.get(&format!("{}/v2/health", base_url))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert!(!res.headers().contains_key("Deprecation"));
    assert!(!res.headers().contains_key("Sunset"));

    // 3. Test backward compatibility route
    let res = client.post(&format!("{}/callback/transaction", base_url))
        .send()
        .await
        .unwrap();

    // In current implementation, callback returns 501 Not Implemented
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
    
    // Test V1 backward compatibility route
    let res = client.post(&format!("{}/v1/callback/transaction", base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
    assert!(res.headers().contains_key("Deprecation"));
}
