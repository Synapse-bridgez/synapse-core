mod common;

use common::{sign_webhook_body, TestApp, TEST_API_KEY};
use serde_json::{json, Value};
use uuid::Uuid;

async fn post(app: &TestApp, payload: Value) -> reqwest::Response {
    let body = serde_json::to_vec(&payload).unwrap();
    let (timestamp, signature) = sign_webhook_body(&body);
    reqwest::Client::new().post(format!("{}/callback", app.base_url))
        .header("Content-Type", "application/json")
        .header("X-Webhook-Timestamp", timestamp)
        .header("X-Webhook-Signature", signature)
        .body(body).send().await.unwrap()
}

fn deposit(anchor: &str, amount: &str) -> Value {
    json!({"stellar_account": format!("G{}", "A".repeat(55)), "amount": amount,
        "asset_code": "TST", "anchor_transaction_id": anchor,
        "callback_type": "deposit", "callback_status": "completed"})
}

#[tokio::test]
#[ignore = "Requires Docker/Postgres and Redis"]
async fn signed_callbacks_create_one_atomic_tenant_owned_order() {
    let app = TestApp::new().await;
    app.set_ready().await;
    let tenant: Uuid = sqlx::query_scalar("UPDATE tenants SET stellar_account=$1 WHERE api_key_hash=$2 RETURNING tenant_id")
        .bind(format!("G{}", "A".repeat(55)))
        .bind(synapse_core::db::queries::hash_api_key(TEST_API_KEY))
        .fetch_one(&app.pool).await.unwrap();
    sqlx::query("INSERT INTO assets (asset_code, asset_issuer, enabled, min_amount, max_amount) VALUES ('TST', 'test-issuer', true, 2, 100)")
        .execute(&app.pool).await.unwrap();
    sqlx::query("UPDATE feature_flags SET enabled=true, rollout_percentage=100 WHERE name='swap_engine'")
        .execute(&app.pool).await.unwrap();

    let (first, retry) = tokio::join!(post(&app, deposit("duplicate", "10")), post(&app, deposit("duplicate", "10.00")));
    assert_eq!(first.status(), 201);
    assert_eq!(retry.status(), 201);
    let first: Value = first.json().await.unwrap();
    let retry: Value = retry.json().await.unwrap();
    assert_eq!(first["id"], retry["id"]);
    let source = Uuid::parse_str(first["id"].as_str().unwrap()).unwrap();
    let order = synapse_core::swap::deposits::order_for_transaction(&app.pool, tenant, source)
        .await.unwrap().unwrap();
    assert_eq!(order.status, "swap_ready");
    assert_eq!(order.source_transaction_id, source);
    assert_eq!(order.tenant_id, tenant);
    let counts: (i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM transactions WHERE id=$1), (SELECT count(*) FROM swap_orders WHERE source_transaction_id=$1)")
        .bind(source).fetch_one(&app.pool).await.unwrap();
    assert_eq!(counts, (1, 1));
    assert!(synapse_core::swap::deposits::order_for_transaction(&app.pool, Uuid::new_v4(), source).await.unwrap().is_none());
    assert_eq!(post(&app, deposit("duplicate", "11")).await.status(), 400);

    let mut pending = deposit("upgrade", "10");
    pending["callback_status"] = json!("pending");
    let before: Value = post(&app, pending.clone()).await.json().await.unwrap();
    let after: Value = post(&app, deposit("upgrade", "10")).await.json().await.unwrap();
    assert_eq!(before["id"], after["id"]);
    assert_eq!(after["status"], "pending");
    let old_retry: Value = post(&app, pending).await.json().await.unwrap();
    assert_eq!(old_retry["callback_status"], "completed");

    for (anchor, amount) in [("below", "1"), ("above", "101")] {
        let response = post(&app, deposit(anchor, amount)).await;
        assert_eq!(response.status(), 201);
        let source: Value = response.json().await.unwrap();
        let source = Uuid::parse_str(source["id"].as_str().unwrap()).unwrap();
        assert!(synapse_core::swap::deposits::order_for_transaction(&app.pool, tenant, source).await.unwrap().is_none());
    }
    for code in ["BAD", "TST"] {
        sqlx::query("UPDATE assets SET enabled=false WHERE asset_code=$1").bind(code).execute(&app.pool).await.unwrap();
        let mut payload = deposit(code, "10");
        payload["asset_code"] = json!(code);
        let response: Value = post(&app, payload).await.json().await.unwrap();
        let source = Uuid::parse_str(response["id"].as_str().unwrap()).unwrap();
        assert!(synapse_core::swap::deposits::order_for_transaction(&app.pool, tenant, source).await.unwrap().is_none());
    }
    sqlx::query("UPDATE assets SET enabled=true WHERE asset_code='TST'").execute(&app.pool).await.unwrap();
    sqlx::query("UPDATE feature_flags SET enabled=false WHERE name='swap_engine'").execute(&app.pool).await.unwrap();
    let gated: Value = post(&app, deposit("gated", "10")).await.json().await.unwrap();
    let gated_id = Uuid::parse_str(gated["id"].as_str().unwrap()).unwrap();
    assert!(synapse_core::swap::deposits::order_for_transaction(&app.pool, tenant, gated_id).await.unwrap().is_none());
    sqlx::query("UPDATE feature_flags SET enabled=true WHERE name='swap_engine'").execute(&app.pool).await.unwrap();
    let replay: Value = post(&app, deposit("gated", "10")).await.json().await.unwrap();
    assert_eq!(gated["id"], replay["id"]);
    assert!(synapse_core::swap::deposits::order_for_transaction(&app.pool, tenant, gated_id).await.unwrap().is_some());

    // Inject an order-write failure to verify no partial Phase 1 write survives.
    sqlx::query("ALTER TABLE swap_orders ADD CONSTRAINT test_order_failure CHECK (amount <> 17)")
        .execute(&app.pool).await.unwrap();
    assert_eq!(post(&app, deposit("rollback", "17")).await.status(), 500);
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM transactions WHERE anchor_transaction_id='rollback'")
        .fetch_one(&app.pool).await.unwrap();
    assert_eq!(remaining, 0);
    sqlx::query("ALTER TABLE swap_orders DROP CONSTRAINT test_order_failure").execute(&app.pool).await.unwrap();
    assert_eq!(post(&app, deposit("rollback", "17")).await.status(), 201);
}
