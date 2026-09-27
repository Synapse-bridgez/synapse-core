mod support;

use support::{spawn_mock_server, stub_get_endpoint};

#[tokio::test]
async fn example_mock_server_stub_and_request() {
    let server = spawn_mock_server().await;
    let path = "/test/endpoint";
    let response = serde_json::json!({
        "id": "123",
        "name": "test",
        "status": "active"
    });

    stub_get_endpoint(&server, path, 200, response.clone()).await;

    let url = format!("{}{}", server.uri(), path);
    let client = reqwest::Client::new();
    let result = client.get(&url).send().await;

    assert!(result.is_ok());
    let resp = result.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["id"], "123");
    assert_eq!(body["status"], "active");
}
