//! Integration tests for issue #947: core transaction/settlement read routes require authentication.
//!
//! Verifies that GET /transactions, /transactions/:id, /transactions/search,
//! /settlements, and /settlements/:id all enforce authentication middleware.

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use reqwest::Client;
    use synapse_core::create_app;
    use tower::ServiceExt;

    /// Helper to make an unauthenticated request to a route
    async fn make_unauthenticated_request(
        app: &axum::Router,
        method: &str,
        path: &str,
    ) -> StatusCode {
        let uri = format!("{}", path).parse().unwrap();
        let req = match method {
            "GET" => Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
            _ => panic!("Unsupported method: {}", method),
        };

        match app.clone().oneshot(req).await {
            Ok(res) => res.status(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Helper to make an authenticated request with an API key
    async fn make_authenticated_request(
        app: &axum::Router,
        method: &str,
        path: &str,
        api_key: &str,
    ) -> StatusCode {
        let uri = format!("{}", path).parse().unwrap();
        let req = match method {
            "GET" => Request::builder()
                .method("GET")
                .uri(uri)
                .header("Authorization", format!("Bearer {}", api_key))
                .body(Body::empty())
                .unwrap(),
            _ => panic!("Unsupported method: {}", method),
        };

        match app.clone().oneshot(req).await {
            Ok(res) => res.status(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Test that /transactions requires authentication
    #[tokio::test]
    async fn transactions_route_requires_auth() {
        // Note: This test documents the requirement from issue #947.
        // When the fix is applied, unauthenticated requests to /transactions
        // should receive a 401 Unauthorized response instead of 200 OK.

        let app = create_app(Default::default()).await.unwrap();

        // Unauthenticated request should be rejected
        let status = make_unauthenticated_request(&app, "GET", "/transactions").await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "GET /transactions without auth should return 401 Unauthorized, \
             per issue #947 fix: core routes must require authentication"
        );
    }

    /// Test that /transactions/:id requires authentication
    #[tokio::test]
    async fn transaction_by_id_requires_auth() {
        let app = create_app(Default::default()).await.unwrap();

        // Unauthenticated request should be rejected
        let status = make_unauthenticated_request(
            &app,
            "GET",
            "/transactions/f47ac10b-58cc-4372-a567-0e02b2c3d479",
        )
        .await;

        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "GET /transactions/:id without auth should return 401 Unauthorized, \
             per issue #947 fix: core routes must require authentication"
        );
    }

    /// Test that /transactions/search requires authentication
    #[tokio::test]
    async fn transactions_search_requires_auth() {
        let app = create_app(Default::default()).await.unwrap();

        // Unauthenticated request should be rejected
        let status = make_unauthenticated_request(&app, "GET", "/transactions/search?q=test").await;

        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "GET /transactions/search without auth should return 401 Unauthorized, \
             per issue #947 fix: core routes must require authentication"
        );
    }

    /// Test that /settlements requires authentication
    #[tokio::test]
    async fn settlements_route_requires_auth() {
        let app = create_app(Default::default()).await.unwrap();

        // Unauthenticated request should be rejected
        let status = make_unauthenticated_request(&app, "GET", "/settlements").await;

        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "GET /settlements without auth should return 401 Unauthorized, \
             per issue #947 fix: core routes must require authentication"
        );
    }

    /// Test that /settlements/:id requires authentication
    #[tokio::test]
    async fn settlement_by_id_requires_auth() {
        let app = create_app(Default::default()).await.unwrap();

        // Unauthenticated request should be rejected
        let status = make_unauthenticated_request(
            &app,
            "GET",
            "/settlements/f47ac10b-58cc-4372-a567-0e02b2c3d479",
        )
        .await;

        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "GET /settlements/:id without auth should return 401 Unauthorized, \
             per issue #947 fix: core routes must require authentication"
        );
    }
}
