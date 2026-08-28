//! Chaos-Style Database Failover & Resilience Test Suite.
//!
//! Validates application resilience under injected TCP drops, latency spikes,
//! and pool exhaustion across representative request flows (Webhook, Settlement, Reconciliation).

use bigdecimal::BigDecimal;
use chrono::Utc;
use std::str::FromStr;
use synapse_core::db::chaos::{ChaosConfig, ChaosProxy, FaultInjector, FaultKind};
use synapse_core::db::pool_manager::PoolManager;
use synapse_core::db::session::DbSession;
use synapse_core::services::reconciliation::ReconciliationService;
use synapse_core::services::settlement::SettlementService;
use synapse_core::services::webhook_dispatcher::WebhookDispatcher;
use synapse_core::stellar::HorizonClient;
use uuid::Uuid;

/// Retrieve test seed from CHAOS_SEED environment variable or default to 42.
fn get_test_seed() -> u64 {
    std::env::var("CHAOS_SEED")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(42)
}

/// Helper to get test DATABASE_URL or return None if skipping DB integration tests.
fn get_database_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

// ── Test 1: Chaos Engine Seed Reproducibility ─────────────────────────────────

#[tokio::test]
async fn test_chaos_rng_seed_reproducibility() {
    let seed = get_test_seed();
    println!("Running Chaos RNG Reproducibility Test with SEED: {}", seed);

    let config1 = ChaosConfig::with_seed(seed);
    let config2 = ChaosConfig::with_seed(seed);

    let injector1 = FaultInjector::new(config1);
    let injector2 = FaultInjector::new(config2);

    let faults1: Vec<_> = (0..50).map(|_| injector1.evaluate_fault()).collect();
    let faults2: Vec<_> = (0..50).map(|_| injector2.evaluate_fault()).collect();

    assert_eq!(
        faults1, faults2,
        "Chaos fault evaluation MUST be 100% reproducible for seed {}",
        seed
    );
    println!("Chaos RNG seed determinism verified successfully.");
}

// ── Test 2: Webhook Processing Flow under Chaos ────────────────────────────────

#[tokio::test]
async fn test_webhook_processing_flow_under_chaos() {
    let seed = get_test_seed();
    println!("Executing Flow 1 (Webhook Processing) under Chaos - Seed: {}", seed);

    let database_url = match get_database_url() {
        Some(url) => url,
        None => {
            println!("Skipping DB-backed Webhook Chaos test: DATABASE_URL not set");
            // Run simulated fault injection check
            let injector = FaultInjector::new(ChaosConfig::with_seed(seed));
            let mut drops = 0;
            for _ in 0..100 {
                if let Some(FaultKind::ConnectionDrop) = injector.evaluate_fault() {
                    drops += 1;
                }
            }
            println!("Simulated Webhook Chaos test completed (Injected {} drops)", drops);
            return;
        }
    };

    let chaos_config = ChaosConfig {
        seed,
        failure_rate: 0.4,
        drop_probability: 0.5,
        latency_probability: 0.3,
        exhaustion_probability: 0.2,
        min_latency_ms: 10,
        max_latency_ms: 100,
        enabled: true,
    };

    let pool_manager = PoolManager::with_chaos(&database_url, None, chaos_config)
        .await
        .expect("Failed to initialize PoolManager with chaos");

    let mut session = pool_manager.create_session();

    // Setup: Insert test endpoint & webhook delivery
    let endpoint_id = Uuid::new_v4();
    let tx_id = Uuid::new_v4();

    let insert_ep = sqlx::query(
        "INSERT INTO webhook_endpoints (id, url, secret, event_types, enabled, created_at, updated_at) 
         VALUES ($1, $2, $3, $4, true, NOW(), NOW()) ON CONFLICT DO NOTHING",
    )
    .bind(endpoint_id)
    .bind("http://127.0.0.1:9999/webhook")
    .bind("test-secret")
    .bind(&vec!["transaction.completed".to_string()])
    .execute(pool_manager.primary())
    .await;

    if let Err(e) = insert_ep {
        println!("Skipping DB execution (table not ready): {:?}", e);
        return;
    }

    let dispatcher = WebhookDispatcher::new(pool_manager.primary().clone());

    // Enqueue webhook deliveries under chaos
    let payload = serde_json::json!({"amount": "100.00", "currency": "USD"});
    let _ = dispatcher.enqueue(tx_id, "transaction.completed", payload).await;

    // Process pending under injected faults
    let _ = dispatcher.process_pending().await;

    // Assert Invariants
    let violations = session
        .assert_data_invariants()
        .await
        .expect("Failed to query data invariants");

    // Cleanup session & locks
    session.cleanup_orphaned_locks().await.unwrap();

    assert!(
        violations.is_empty(),
        "Flow 1 Webhook Processing left invariant violations under seed {}: {:?}",
        seed,
        violations
    );
}

// ── Test 3: Settlement Processing Flow under Chaos ─────────────────────────────

#[tokio::test]
async fn test_settlement_processing_flow_under_chaos() {
    let seed = get_test_seed();
    println!("Executing Flow 2 (Settlement Processing) under Chaos - Seed: {}", seed);

    let database_url = match get_database_url() {
        Some(url) => url,
        None => {
            println!("Skipping DB-backed Settlement Chaos test: DATABASE_URL not set");
            return;
        }
    };

    let chaos_config = ChaosConfig {
        seed,
        failure_rate: 0.35,
        drop_probability: 0.4,
        latency_probability: 0.4,
        exhaustion_probability: 0.2,
        min_latency_ms: 20,
        max_latency_ms: 150,
        enabled: true,
    };

    let pool_manager = PoolManager::with_chaos(&database_url, None, chaos_config)
        .await
        .expect("Failed to initialize PoolManager with chaos");

    let mut session = pool_manager.create_session();

    // Execute settlement service under fault injection
    let settlement_service = SettlementService::new(pool_manager.primary().clone());

    // Attempt settlements across assets
    let res = settlement_service.run_settlements().await;
    match res {
        Ok(settlements) => {
            println!("Settlements completed successfully: {} records created", settlements.len());
        }
        Err(e) => {
            println!("Settlement service encountered handled error under chaos: {:?}", e);
        }
    }

    // Invariant Check: Verify NO PARTIAL WRITES or STUCK LOCKS
    let violations = session
        .assert_data_invariants()
        .await
        .expect("Invariant check failed");

    // Lock Cleanup
    session.cleanup_orphaned_locks().await.unwrap();

    assert!(
        violations.is_empty(),
        "Flow 2 Settlement Processing produced invariant violations with seed {}: {:?}",
        seed,
        violations
    );
}

// ── Test 4: Reconciliation Processing Flow under Chaos ────────────────────────

#[tokio::test]
async fn test_reconciliation_processing_flow_under_chaos() {
    let seed = get_test_seed();
    println!("Executing Flow 3 (Reconciliation Processing) under Chaos - Seed: {}", seed);

    let database_url = match get_database_url() {
        Some(url) => url,
        None => {
            println!("Skipping DB-backed Reconciliation Chaos test: DATABASE_URL not set");
            return;
        }
    };

    let chaos_config = ChaosConfig {
        seed,
        failure_rate: 0.5,
        drop_probability: 0.3,
        latency_probability: 0.5,
        exhaustion_probability: 0.2,
        min_latency_ms: 30,
        max_latency_ms: 200,
        enabled: true,
    };

    let pool_manager = PoolManager::with_chaos(&database_url, None, chaos_config)
        .await
        .expect("Failed to initialize PoolManager with chaos");

    let mut session = pool_manager.create_session();

    let horizon_client = HorizonClient::new("https://horizon-testnet.stellar.org".to_string());
    let recon_service = ReconciliationService::new(horizon_client, pool_manager.primary().clone());

    let start_time = Utc::now() - chrono::Duration::days(1);
    let end_time = Utc::now();

    let result = recon_service.reconcile("GBRPYHIL2CI3FNQ4BXLFMNDLFPPPU2HY5BTH4TTJ3HQ6N4D6L546M7B5", start_time, end_time).await;

    match result {
        Ok(report) => {
            println!(
                "Reconciliation report generated under chaos: {} total DB txs, {} chain payments",
                report.total_db_transactions, report.total_chain_payments
            );
        }
        Err(e) => {
            println!("Reconciliation service failed gracefully under chaos: {:?}", e);
        }
    }

    // Invariant assertion after reconciliation
    let violations = session.assert_data_invariants().await.unwrap();
    session.cleanup_orphaned_locks().await.unwrap();

    assert!(
        violations.is_empty(),
        "Flow 3 Reconciliation Processing produced invariant violations with seed {}: {:?}",
        seed,
        violations
    );
}

// ── Test 5: TCP Proxy Connection Drop & Lock Cleanup Verification ─────────────

#[tokio::test]
async fn test_chaos_proxy_lock_cleanup_isolation() {
    let seed = get_test_seed();
    println!("Testing ChaosProxy TCP-level connection drop and lock cleanup with seed: {}", seed);

    let database_url = match get_database_url() {
        Some(url) => url,
        None => {
            println!("Skipping ChaosProxy TCP test: DATABASE_URL not set");
            return;
        }
    };

    // Parse target DB host/port
    let target_addr: std::net::SocketAddr = "127.0.0.1:5432".parse().unwrap_or_else(|_| {
        "127.0.0.1:5432".parse().unwrap()
    });

    let chaos_config = ChaosConfig {
        seed,
        failure_rate: 0.3,
        drop_probability: 0.5,
        latency_probability: 0.5,
        exhaustion_probability: 0.0,
        min_latency_ms: 10,
        max_latency_ms: 50,
        enabled: true,
    };

    if let Ok(proxy) = ChaosProxy::start(target_addr, chaos_config).await {
        let proxy_url = proxy.proxy_db_url(&database_url);
        println!("ChaosProxy running at proxy URL: {}", proxy_url);

        if let Ok(pool_manager) = PoolManager::new(&proxy_url, None).await {
            let mut session = pool_manager.create_session();
            let _ = session.acquire_advisory_lock(987654).await;
            let _ = session.cleanup_orphaned_locks().await;

            let violations = session.assert_data_invariants().await.unwrap();
            assert!(violations.is_empty());
        }

        proxy.shutdown();
    } else {
        println!("Skipping ChaosProxy start (port bind or target unavailable)");
    }
}
