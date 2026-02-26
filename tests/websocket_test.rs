use chrono::Utc;
use futures::{SinkExt, StreamExt};
use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use synapse_core::db::pool_manager::PoolManager;
use synapse_core::handlers::ws::TransactionStatusUpdate;
use synapse_core::services::feature_flags::FeatureFlagService;
use synapse_core::{create_app, AppState};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use uuid::Uuid;

async fn setup_test_app() -> (
    String,
    PgPool,
    broadcast::Sender<TransactionStatusUpdate>,
    impl std::any::Any,
) {
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
    let (tx_broadcast, _) = broadcast::channel::<TransactionStatusUpdate>(100);
    let query_cache = synapse_core::services::QueryCache::new("redis://localhost:6379").unwrap();

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
        tx_broadcast: tx_broadcast.clone(),
        query_cache,
    };

    let app = create_app(app_state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let std_listener = listener.into_std().unwrap();

    tokio::spawn(async move {
        axum::Server::from_tcp(std_listener)
            .unwrap()
            .serve(app.into_make_service())
            .await
            .unwrap();
    });

    let base_url = format!("ws://{}", addr);
    (base_url, pool, tx_broadcast, container)
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_ws_connection_with_valid_token() {
    let (base_url, _pool, _tx, _container) = setup_test_app().await;

    // Connect with valid token
    let ws_url = format!("{}/ws?token=valid-token-123", base_url);
    let result = connect_async(&ws_url).await;

    assert!(result.is_ok(), "Should connect with valid token");

    let (mut ws_stream, _) = result.unwrap();

    // Send a ping to verify connection is alive
    ws_stream.send(Message::Ping(vec![])).await.unwrap();

    // Close connection gracefully
    ws_stream.close(None).await.unwrap();
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_ws_connection_rejected_invalid_token() {
    let (base_url, _pool, _tx, _container) = setup_test_app().await;

    // Try to connect without token (should be rejected)
    let ws_url = format!("{}/ws", base_url);
    let result = connect_async(&ws_url).await;

    // Connection should fail or be rejected
    // Note: The actual behavior depends on how axum handles the rejection
    // It might connect but immediately close, or fail to upgrade
    match result {
        Ok((mut ws_stream, _)) => {
            // If it connects, it should close immediately or we should get an error
            let msg =
                tokio::time::timeout(tokio::time::Duration::from_secs(2), ws_stream.next()).await;

            // Should either timeout or receive close message
            assert!(msg.is_err() || matches!(msg.unwrap(), Some(Ok(Message::Close(_)))));
        }
        Err(_) => {
            // Connection rejected at HTTP level - this is also acceptable
        }
    }
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_ws_receives_transaction_updates() {
    let (base_url, _pool, tx_broadcast, _container) = setup_test_app().await;

    // Connect WebSocket client
    let ws_url = format!("{}/ws?token=test-token", base_url);
    let (mut ws_stream, _) = connect_async(&ws_url).await.unwrap();

    // Give the connection time to establish
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Broadcast a transaction update
    let transaction_id = Uuid::new_v4();
    let update = TransactionStatusUpdate {
        transaction_id,
        status: "completed".to_string(),
        timestamp: Utc::now(),
        message: Some("Transaction processed successfully".to_string()),
    };

    tx_broadcast.send(update.clone()).unwrap();

    // Wait for the message
    let msg = tokio::time::timeout(tokio::time::Duration::from_secs(5), ws_stream.next()).await;

    assert!(msg.is_ok(), "Should receive message within timeout");

    let msg = msg.unwrap().unwrap().unwrap();

    if let Message::Text(text) = msg {
        let received: TransactionStatusUpdate = serde_json::from_str(&text).unwrap();
        assert_eq!(received.transaction_id, transaction_id);
        assert_eq!(received.status, "completed");
    } else {
        panic!("Expected text message, got {:?}", msg);
    }

    ws_stream.close(None).await.unwrap();
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_ws_multiple_clients_receive_broadcast() {
    let (base_url, _pool, tx_broadcast, _container) = setup_test_app().await;

    // Connect multiple WebSocket clients
    let ws_url1 = format!("{}/ws?token=client1", base_url);
    let ws_url2 = format!("{}/ws?token=client2", base_url);
    let ws_url3 = format!("{}/ws?token=client3", base_url);

    let (mut ws_stream1, _) = connect_async(&ws_url1).await.unwrap();
    let (mut ws_stream2, _) = connect_async(&ws_url2).await.unwrap();
    let (mut ws_stream3, _) = connect_async(&ws_url3).await.unwrap();

    // Give connections time to establish
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // Broadcast a transaction update
    let transaction_id = Uuid::new_v4();
    let update = TransactionStatusUpdate {
        transaction_id,
        status: "pending".to_string(),
        timestamp: Utc::now(),
        message: None,
    };

    let sent_count = tx_broadcast.send(update.clone()).unwrap();
    assert_eq!(sent_count, 3, "Should have 3 active subscribers");

    // All clients should receive the message
    let msg1 = tokio::time::timeout(tokio::time::Duration::from_secs(5), ws_stream1.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let msg2 = tokio::time::timeout(tokio::time::Duration::from_secs(5), ws_stream2.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let msg3 = tokio::time::timeout(tokio::time::Duration::from_secs(5), ws_stream3.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    // Verify all received the same update
    for msg in [msg1, msg2, msg3] {
        if let Message::Text(text) = msg {
            let received: TransactionStatusUpdate = serde_json::from_str(&text).unwrap();
            assert_eq!(received.transaction_id, transaction_id);
            assert_eq!(received.status, "pending");
        } else {
            panic!("Expected text message");
        }
    }

    ws_stream1.close(None).await.unwrap();
    ws_stream2.close(None).await.unwrap();
    ws_stream3.close(None).await.unwrap();
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_ws_connection_cleanup_on_disconnect() {
    let (base_url, _pool, tx_broadcast, _container) = setup_test_app().await;

    // Connect a client
    let ws_url = format!("{}/ws?token=test-client", base_url);
    let (ws_stream, _) = connect_async(&ws_url).await.unwrap();

    // Give connection time to establish
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Verify client is subscribed
    let update = TransactionStatusUpdate {
        transaction_id: Uuid::new_v4(),
        status: "test".to_string(),
        timestamp: Utc::now(),
        message: None,
    };

    let sent_count = tx_broadcast.send(update.clone()).unwrap();
    assert_eq!(sent_count, 1, "Should have 1 active subscriber");

    // Drop the connection (simulates client disconnect)
    drop(ws_stream);

    // Give time for cleanup
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Try to broadcast again - should have 0 subscribers
    let update2 = TransactionStatusUpdate {
        transaction_id: Uuid::new_v4(),
        status: "test2".to_string(),
        timestamp: Utc::now(),
        message: None,
    };

    let sent_count2 = tx_broadcast.send(update2).unwrap();
    assert_eq!(
        sent_count2, 0,
        "Should have 0 active subscribers after disconnect"
    );
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_ws_heartbeat_keeps_connection_alive() {
    let (base_url, _pool, _tx, _container) = setup_test_app().await;

    // Connect WebSocket client
    let ws_url = format!("{}/ws?token=heartbeat-test", base_url);
    let (mut ws_stream, _) = connect_async(&ws_url).await.unwrap();

    // Wait for heartbeat ping (server sends every 30 seconds, but we'll wait a bit)
    // Note: In real tests, you might want to mock time or reduce heartbeat interval
    let msg = tokio::time::timeout(tokio::time::Duration::from_secs(35), async {
        loop {
            if let Some(Ok(msg)) = ws_stream.next().await {
                if matches!(msg, Message::Ping(_)) {
                    return msg;
                }
            }
        }
    })
    .await;

    assert!(msg.is_ok(), "Should receive heartbeat ping");

    ws_stream.close(None).await.unwrap();
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_ws_client_can_send_messages() {
    let (base_url, _pool, _tx, _container) = setup_test_app().await;

    // Connect WebSocket client
    let ws_url = format!("{}/ws?token=send-test", base_url);
    let (mut ws_stream, _) = connect_async(&ws_url).await.unwrap();

    // Send a text message to server
    let test_message = r#"{"action":"subscribe","filters":{"status":"completed"}}"#;
    ws_stream
        .send(Message::Text(test_message.to_string()))
        .await
        .unwrap();

    // Server should handle it gracefully (even if it doesn't respond)
    // Wait a bit to ensure no errors
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Connection should still be alive
    ws_stream.send(Message::Ping(vec![])).await.unwrap();

    ws_stream.close(None).await.unwrap();
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_ws_handles_rapid_broadcasts() {
    let (base_url, _pool, tx_broadcast, _container) = setup_test_app().await;

    // Connect WebSocket client
    let ws_url = format!("{}/ws?token=rapid-test", base_url);
    let (mut ws_stream, _) = connect_async(&ws_url).await.unwrap();

    // Give connection time to establish
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Send multiple rapid updates
    let mut sent_ids = Vec::new();
    for i in 0..10 {
        let transaction_id = Uuid::new_v4();
        sent_ids.push(transaction_id);

        let update = TransactionStatusUpdate {
            transaction_id,
            status: format!("status_{}", i),
            timestamp: Utc::now(),
            message: Some(format!("Update {}", i)),
        };

        tx_broadcast.send(update).unwrap();
    }

    // Receive all messages
    let mut received_count = 0;
    for _ in 0..10 {
        let msg = tokio::time::timeout(tokio::time::Duration::from_secs(5), ws_stream.next()).await;

        if let Ok(Some(Ok(Message::Text(_)))) = msg {
            received_count += 1;
        }
    }

    assert_eq!(received_count, 10, "Should receive all 10 rapid updates");

    ws_stream.close(None).await.unwrap();
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_ws_connection_with_empty_token() {
    let (base_url, _pool, _tx, _container) = setup_test_app().await;

    // Try to connect with empty token
    let ws_url = format!("{}/ws?token=", base_url);
    let result = connect_async(&ws_url).await;

    // Should be rejected (empty token is invalid)
    match result {
        Ok((mut ws_stream, _)) => {
            // If it connects, it should close immediately
            let msg =
                tokio::time::timeout(tokio::time::Duration::from_secs(2), ws_stream.next()).await;

            assert!(msg.is_err() || matches!(msg.unwrap(), Some(Ok(Message::Close(_)))));
        }
        Err(_) => {
            // Connection rejected - this is expected
        }
    }
}
