use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};

pub async fn admin_auth(req: Request<Body>, next: Next<Body>) -> Result<Response, StatusCode> {
    let auth_header = req
        .headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok());

    let admin_api_key = std::env::var("ADMIN_API_KEY").unwrap_or_else(|_| "admin-secret-key".to_string());

    match auth_header {
        Some(auth) if auth == format!("Bearer {}", admin_api_key) || auth == admin_api_key => {
            Ok(next.run(req).await)
        }
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}
