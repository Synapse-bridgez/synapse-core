use assert_cmd::Command;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn synapse_cmd() -> Command {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("synapse-core");
    cmd.envs([
        (
            "DATABASE_URL",
            "postgres://synapse:synapse@localhost:5433/synapse_test",
        ),
        ("STELLAR_HORIZON_URL", "https://horizon-testnet.stellar.org"),
        ("REDIS_URL", "redis://localhost:6379"),
        ("VAULT_URL", "http://localhost:8200"),
        ("VAULT_TOKEN", "root"),
        ("ENVIRONMENT", "testing"),
    ]);
    cmd
}

#[tokio::test]
#[ignore = "Requires mock server setup"]
async fn test_transactions_search_table_format() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/transactions/search"))
        .and(header("X-API-Key", "dev-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total": 1,
            "results": [
                {
                    "id": "550e8400-e29b-41d4-a716-446655440000",
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
                }
            ],
            "next_cursor": null
        })))
        .mount(&server)
        .await;

    let mut cmd = synapse_cmd();
    cmd.env("SYNAPSE_API_URL", server.uri());
    cmd.arg("tx")
        .arg("search")
        .arg("--asset-code")
        .arg("USD")
        .arg("--format")
        .arg("table");

    cmd.assert().success();
}

#[tokio::test]
#[ignore = "Requires mock server setup"]
async fn test_transactions_search_json_format() {
    let server = MockServer::start().await;

    let search_response = json!({
        "total": 1,
        "results": [
            {
                "id": "550e8400-e29b-41d4-a716-446655440000",
                "stellar_account": "GABC1234567890123456789012345678901234567890123456789012",
                "amount": "100.00",
                "asset_code": "USD",
                "status": "completed",
                "created_at": "2024-01-15T10:00:00Z",
                "updated_at": "2024-01-15T10:00:00Z",
                "anchor_transaction_id": null,
                "callback_type": null,
                "callback_status": null,
                "settlement_id": null,
                "memo": null,
                "memo_type": null,
                "metadata": null
            }
        ],
        "next_cursor": null
    });

    Mock::given(method("GET"))
        .and(path("/transactions/search"))
        .and(header("X-API-Key", "dev-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&search_response))
        .mount(&server)
        .await;

    let mut cmd = synapse_cmd();
    cmd.env("SYNAPSE_API_URL", server.uri());
    cmd.arg("tx")
        .arg("search")
        .arg("--status")
        .arg("completed")
        .arg("--format")
        .arg("json");

    cmd.assert().success();
}

#[tokio::test]
#[ignore = "Requires mock server setup"]
async fn test_transactions_search_with_pagination() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/transactions/search"))
        .and(header("X-API-Key", "dev-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total": 50,
            "results": [
                {
                    "id": "550e8400-e29b-41d4-a716-446655440000",
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
                }
            ],
            "next_cursor": "eyJpZCI6IjU1MGU4NDAwLWUyOWItNDFkNC1hNzE2LTQ0NjY1NTQ0MDAwMCIsImNyZWF0ZWRfYXQiOiIyMDI0LTAxLTE1VDEwOjAwOjAwWiJ9"
        })))
        .mount(&server)
        .await;

    let mut cmd = synapse_cmd();
    cmd.env("SYNAPSE_API_URL", server.uri());
    cmd.arg("tx")
        .arg("search")
        .arg("--min-amount")
        .arg("50.00")
        .arg("--max-amount")
        .arg("200.00")
        .arg("--limit")
        .arg("25");

    cmd.assert().success();
}

#[tokio::test]
#[ignore = "Requires mock server setup"]
async fn test_transactions_search_empty_results() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/transactions/search"))
        .and(header("X-API-Key", "dev-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total": 0,
            "results": [],
            "next_cursor": null
        })))
        .mount(&server)
        .await;

    let mut cmd = synapse_cmd();
    cmd.env("SYNAPSE_API_URL", server.uri());
    cmd.arg("tx")
        .arg("search")
        .arg("--asset-code")
        .arg("NONEXISTENT");

    cmd.assert().success();
}

#[tokio::test]
#[ignore = "Requires mock server setup"]
async fn test_transactions_search_with_all_filters() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/transactions/search"))
        .and(header("X-API-Key", "dev-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total": 1,
            "results": [
                {
                    "id": "550e8400-e29b-41d4-a716-446655440000",
                    "stellar_account": "GBBD47FW5DWKKQZC2V4LLSAHX5VJKJ2EUYJ7YIDUPBBVNHYF7LOHYV7O",
                    "amount": "123.45",
                    "asset_code": "EUR",
                    "status": "processing",
                    "created_at": "2024-02-01T15:30:00Z",
                    "updated_at": "2024-02-01T15:30:00Z",
                    "anchor_transaction_id": null,
                    "callback_type": null,
                    "callback_status": null,
                    "settlement_id": null,
                    "memo": null,
                    "memo_type": null,
                    "metadata": null
                }
            ],
            "next_cursor": null
        })))
        .mount(&server)
        .await;

    let mut cmd = synapse_cmd();
    cmd.env("SYNAPSE_API_URL", server.uri());
    cmd.arg("tx")
        .arg("search")
        .arg("--status")
        .arg("processing")
        .arg("--asset-code")
        .arg("EUR")
        .arg("--min-amount")
        .arg("100.00")
        .arg("--max-amount")
        .arg("200.00")
        .arg("--from")
        .arg("2024-02-01T00:00:00Z")
        .arg("--to")
        .arg("2024-02-02T00:00:00Z")
        .arg("--stellar-account")
        .arg("GBBD47FW5DWKKQZC2V4LLSAHX5VJKJ2EUYJ7YIDUPBBVNHYF7LOHYV7O");

    cmd.assert().success();
}
