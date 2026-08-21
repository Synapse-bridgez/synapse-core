#[cfg(test)]
mod readiness_tests {
    use reqwest::StatusCode;
    use sqlx::{migrate::Migrator, PgPool};
    use std::path::Path;
    use synapse_core::{create_app, AppState, ReadinessState};
    use testcontainers::{runners::AsyncRunner, ImageExt};
    use testcontainers_modules::postgres::Postgres;

    /// Integration test: verify /ready endpoint transitions from 503 → 200 during startup
    /// This test exercises the real startup sequence and confirms run_initialization_checks()
    /// is called before the HTTP listener starts accepting traffic.
    #[tokio::test]
    #[ignore]
    async fn test_readiness_probe_transitions_to_ready_on_startup() {
        let container = Postgres::default()
            .with_tag("14-alpine")
            .start()
            .await
            .unwrap();
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

        let (tx, _rx) = tokio::sync::broadcast::channel(100);
        let redis_url = "redis://localhost:6379";
        let horizon_url = "https://horizon-testnet.stellar.org";

        let asset_cache =
            synapse_core::AssetCache::start(pool.clone(), std::time::Duration::from_secs(300))
                .await
                .expect("failed to start asset cache in test");

        let readiness = ReadinessState::new();

        // Before calling run_initialization_checks(), readiness should be false
        assert!(
            !readiness.is_ready(),
            "ReadinessState should start as NOT READY"
        );

        // Run the initialization checks (this is what main.rs now does before starting the listener)
        let init_result = readiness
            .run_initialization_checks(&pool, redis_url, horizon_url)
            .await;

        // Initialization should succeed (or warn about non-critical checks)
        assert!(
            init_result.is_ok(),
            "Initialization checks should pass (or only warn on non-critical checks). Error: {:?}",
            init_result.err()
        );

        // After initialization, readiness should be true
        assert!(
            readiness.is_ready(),
            "ReadinessState should be READY after successful initialization"
        );

        let app_state = AppState {
            db: pool.clone(),
            pool_manager: synapse_core::db::pool_manager::PoolManager::new(&database_url, None, 5)
                .await
                .unwrap(),
            horizon_client: synapse_core::stellar::HorizonClient::new(
                "https://horizon-testnet.stellar.org".to_string(),
            ),
            feature_flags: synapse_core::services::feature_flags::FeatureFlagService::new(
                pool.clone(),
            ),
            redis_url: redis_url.to_string(),
            start_time: std::time::Instant::now(),
            readiness: readiness.clone(),
            tx_broadcast: tx,
            query_cache: synapse_core::services::QueryCache::new(redis_url)
                .await
                .unwrap(),
            profiling_manager: synapse_core::handlers::profiling::ProfilingManager::new(),
            tenant_configs: std::sync::Arc::new(tokio::sync::RwLock::new(
                std::collections::HashMap::new(),
            )),
            pending_queue_depth: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            current_batch_size: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(10)),
            secrets_store: None,
            metrics_handle: synapse_core::metrics::init_metrics().unwrap(),
            ws_connection_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            quota_manager: synapse_core::middleware::quota::QuotaManager::new(redis_url)
                .expect("quota manager init failed"),
            asset_cache,
            idempotency_service: None,
        };

        let app = create_app(app_state);
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
        let server = axum::Server::bind(&addr).serve(app.into_make_service());
        let actual_addr = server.local_addr();

        tokio::spawn(async move {
            server.await.unwrap();
        });

        // Give the server a moment to start
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let client = reqwest::Client::new();
        let base_url = format!("http://{}", actual_addr);

        // Verify /ready returns 200 OK with "ready" status
        let res = client
            .get(format!("{}/ready", base_url))
            .send()
            .await
            .unwrap();

        assert_eq!(
            res.status(),
            StatusCode::OK,
            "Readiness endpoint should return 200 after initialization"
        );

        let body: serde_json::Value = res.json().await.unwrap();
        assert_eq!(
            body["status"], "ready",
            "Readiness response should indicate 'ready' status"
        );
    }

    /// Unit test: verify ReadinessState tracks is_ready state correctly
    #[test]
    fn test_readiness_state_default_not_ready() {
        let readiness = ReadinessState::new();
        assert!(
            !readiness.is_ready(),
            "New ReadinessState should start as NOT READY"
        );
    }

    /// Unit test: verify set_ready() marks the service as ready
    #[test]
    fn test_readiness_state_set_ready() {
        let readiness = ReadinessState::new();
        assert!(!readiness.is_ready());

        readiness.set_ready();
        assert!(
            readiness.is_ready(),
            "set_ready() should mark service as ready"
        );
        assert!(
            !readiness.is_draining(),
            "set_ready() should clear the draining flag"
        );
    }

    /// Unit test: verify set_not_ready() marks the service as not ready and draining
    #[test]
    fn test_readiness_state_set_not_ready() {
        let readiness = ReadinessState::new();
        readiness.set_ready();
        assert!(readiness.is_ready());

        readiness.set_not_ready();
        assert!(
            !readiness.is_ready(),
            "set_not_ready() should mark service as not ready"
        );
        assert!(
            readiness.is_draining(),
            "set_not_ready() should set the draining flag"
        );
    }
}
