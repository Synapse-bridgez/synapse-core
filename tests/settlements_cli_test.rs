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
async fn test_settlements_list_table_format() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/settlements"))
        .and(header("X-API-Key", "dev-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "settlements": [
                {
                    "id": "550e8400-e29b-41d4-a716-446655440000",
                    "asset_code": "USD",
                    "total_amount": "1000.00",
                    "tx_count": 5,
                    "status": "pending",
                    "period_start": "2024-01-01T00:00:00Z",
                    "period_end": "2024-01-31T23:59:59Z",
                    "created_at": "2024-01-15T10:00:00Z",
                    "updated_at": "2024-01-15T10:00:00Z",
                    "dispute_reason": null,
                    "original_total_amount": null,
                    "reviewed_by": null,
                    "reviewed_at": null
                }
            ],
            "next_cursor": null,
            "has_more": false
        })))
        .mount(&server)
        .await;

    let mut cmd = synapse_cmd();
    cmd.env("SYNAPSE_API_URL", server.uri());
    cmd.arg("settlements").arg("list").arg("--format").arg("table");

    cmd.assert().success();
}

#[tokio::test]
#[ignore = "Requires mock server setup"]
async fn test_settlements_list_json_format() {
    let server = MockServer::start().await;

    let settlement_json = json!({
        "settlements": [
            {
                "id": "550e8400-e29b-41d4-a716-446655440000",
                "asset_code": "USD",
                "total_amount": "1000.00",
                "tx_count": 5,
                "status": "pending",
                "period_start": "2024-01-01T00:00:00Z",
                "period_end": "2024-01-31T23:59:59Z",
                "created_at": "2024-01-15T10:00:00Z",
                "updated_at": "2024-01-15T10:00:00Z",
                "dispute_reason": null,
                "original_total_amount": null,
                "reviewed_by": null,
                "reviewed_at": null
            }
        ],
        "next_cursor": null,
        "has_more": false
    });

    Mock::given(method("GET"))
        .and(path("/settlements"))
        .and(header("X-API-Key", "dev-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&settlement_json))
        .mount(&server)
        .await;

    let mut cmd = synapse_cmd();
    cmd.env("SYNAPSE_API_URL", server.uri());
    cmd.arg("settlements").arg("list").arg("--format").arg("json");

    cmd.assert().success();
}

#[tokio::test]
#[ignore = "Requires mock server setup"]
async fn test_settlements_get_success() {
    let server = MockServer::start().await;
    let settlement_id = "550e8400-e29b-41d4-a716-446655440000";

    Mock::given(method("GET"))
        .and(path(format!("/settlements/{}", settlement_id)))
        .and(header("X-API-Key", "dev-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": settlement_id,
            "asset_code": "USD",
            "total_amount": "1000.00",
            "tx_count": 5,
            "status": "pending",
            "period_start": "2024-01-01T00:00:00Z",
            "period_end": "2024-01-31T23:59:59Z",
            "created_at": "2024-01-15T10:00:00Z",
            "updated_at": "2024-01-15T10:00:00Z",
            "dispute_reason": null,
            "original_total_amount": null,
            "reviewed_by": null,
            "reviewed_at": null
        })))
        .mount(&server)
        .await;

    let mut cmd = synapse_cmd();
    cmd.env("SYNAPSE_API_URL", server.uri());
    cmd.arg("settlements").arg("get").arg(settlement_id);

    cmd.assert().success();
}

#[tokio::test]
#[ignore = "Requires mock server setup"]
async fn test_settlements_get_json_format() {
    let server = MockServer::start().await;
    let settlement_id = "550e8400-e29b-41d4-a716-446655440000";

    Mock::given(method("GET"))
        .and(path(format!("/settlements/{}", settlement_id)))
        .and(header("X-API-Key", "dev-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": settlement_id,
            "asset_code": "USD",
            "total_amount": "1000.00",
            "tx_count": 5,
            "status": "pending",
            "period_start": "2024-01-01T00:00:00Z",
            "period_end": "2024-01-31T23:59:59Z",
            "created_at": "2024-01-15T10:00:00Z",
            "updated_at": "2024-01-15T10:00:00Z",
            "dispute_reason": null,
            "original_total_amount": null,
            "reviewed_by": null,
            "reviewed_at": null
        })))
        .mount(&server)
        .await;

    let mut cmd = synapse_cmd();
    cmd.env("SYNAPSE_API_URL", server.uri());
    cmd.arg("settlements")
        .arg("get")
        .arg(settlement_id)
        .arg("--format")
        .arg("json");

    cmd.assert().success();
}

#[tokio::test]
#[ignore = "Requires mock server setup"]
async fn test_settlements_get_not_found() {
    let server = MockServer::start().await;
    let settlement_id = "00000000-0000-0000-0000-000000000000";

    Mock::given(method("GET"))
        .and(path(format!("/settlements/{}", settlement_id)))
        .and(header("X-API-Key", "dev-key"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Settlement not found"))
        .mount(&server)
        .await;

    let mut cmd = synapse_cmd();
    cmd.env("SYNAPSE_API_URL", server.uri());
    cmd.arg("settlements").arg("get").arg(settlement_id);

    cmd.assert().failure();
}
