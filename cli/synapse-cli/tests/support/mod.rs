use wiremock::MockServer;

pub async fn spawn_mock_server() -> MockServer {
    MockServer::start().await
}

/// Stub a GET endpoint on `server` that returns `status` and `body` as JSON.
pub async fn stub_get_endpoint(
    server: &MockServer,
    path: &str,
    status: u16,
    body: serde_json::Value,
) {
    use wiremock::matchers::{method, path as path_matcher};
    use wiremock::{Mock, ResponseTemplate};

    Mock::given(method("GET"))
        .and(path_matcher(path))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}
