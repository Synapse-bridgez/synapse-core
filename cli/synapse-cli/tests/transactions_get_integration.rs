mod support;

use support::{spawn_mock_server, stub_get_endpoint};

fn transaction_json(id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "stellar_account": "GABC1234567890123456789012345678901234567890123456789012",
        "amount": "100.00",
        "asset_code": "USD",
        "status": "pending",
        "created_at": "2024-01-15T10:00:00Z",
        "updated_at": "2024-01-15T10:00:00Z",
        "anchor_transaction_id": null,
        "callback_type": null,
        "callback_status": null,
        "settlement_id": null,
        "memo": null,
        "memo_type": null,
        "metadata": null
    })
}

#[tokio::test]
async fn transactions_get_returns_table_format_on_success() {
    let server = spawn_mock_server().await;
    let tx_id = "550e8400-e29b-41d4-a716-446655440000";

    stub_get_endpoint(
        &server,
        &format!("/transactions/{}", tx_id),
        200,
        transaction_json(tx_id),
    )
    .await;

    let client = reqwest::Client::new();
    let url = format!("{}/transactions/{}", server.uri(), tx_id);

    let resp = client.get(&url).send().await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(body["id"], tx_id);
    assert_eq!(body["amount"], "100.00");
    assert_eq!(body["asset_code"], "USD");
}

#[tokio::test]
async fn transactions_get_returns_json_format_on_success() {
    let server = spawn_mock_server().await;
    let tx_id = "550e8400-e29b-41d4-a716-446655440000";

    stub_get_endpoint(
        &server,
        &format!("/transactions/{}", tx_id),
        200,
        transaction_json(tx_id),
    )
    .await;

    let client = reqwest::Client::new();
    let url = format!("{}/transactions/{}", server.uri(), tx_id);

    let resp = client.get(&url).send().await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();

    let json_str = serde_json::to_string_pretty(&body).unwrap();
    assert!(json_str.contains(tx_id));
    assert!(json_str.contains("100.00"));
    assert!(json_str.contains("USD"));
}

#[tokio::test]
async fn transactions_get_returns_404_on_not_found() {
    let server = spawn_mock_server().await;
    let tx_id = "00000000-0000-0000-0000-000000000000";

    stub_get_endpoint(
        &server,
        &format!("/transactions/{}", tx_id),
        404,
        serde_json::json!({"error": "not found"}),
    )
    .await;

    let client = reqwest::Client::new();
    let url = format!("{}/transactions/{}", server.uri(), tx_id);

    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 404);
}

#[tokio::test]
async fn transactions_get_handles_500_error() {
    let server = spawn_mock_server().await;
    let tx_id = "550e8400-e29b-41d4-a716-446655440000";

    stub_get_endpoint(
        &server,
        &format!("/transactions/{}", tx_id),
        500,
        serde_json::json!({"error": "internal server error"}),
    )
    .await;

    let client = reqwest::Client::new();
    let url = format!("{}/transactions/{}", server.uri(), tx_id);

    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 500);
}

#[tokio::test]
async fn transactions_get_multiple_stubs_per_path() {
    let server = spawn_mock_server().await;

    let tx1_id = "550e8400-e29b-41d4-a716-446655440001";
    let tx2_id = "550e8400-e29b-41d4-a716-446655440002";

    stub_get_endpoint(
        &server,
        &format!("/transactions/{}", tx1_id),
        200,
        transaction_json(tx1_id),
    )
    .await;

    stub_get_endpoint(
        &server,
        &format!("/transactions/{}", tx2_id),
        200,
        transaction_json(tx2_id),
    )
    .await;

    let client = reqwest::Client::new();

    let resp1 = client
        .get(format!("{}/transactions/{}", server.uri(), tx1_id))
        .send()
        .await
        .unwrap();
    let body1: serde_json::Value = resp1.json().await.unwrap();
    assert_eq!(body1["id"], tx1_id);

    let resp2 = client
        .get(format!("{}/transactions/{}", server.uri(), tx2_id))
        .send()
        .await
        .unwrap();
    let body2: serde_json::Value = resp2.json().await.unwrap();
    assert_eq!(body2["id"], tx2_id);
}
