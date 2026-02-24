use actix_web::{test, web, App};
use chrono::{Duration, Utc};
use jsonwebtoken::{encode, EncodingKey, Header};
use serde_json::json;

use admin_auth_api::{
    handlers::admin,
    middleware::auth::AuthMiddleware,
    models::{Claims, Role},
};

const JWT_SECRET: &[u8] = b"test_secret_key";

fn create_test_token(role: Role, expired: bool) -> String {
    let expiration = if expired {
        Utc::now() - Duration::hours(1)
    } else {
        Utc::now() + Duration::hours(1)
    };

    let claims = Claims {
        sub: "test_user".to_string(),
        role,
        exp: expiration.timestamp() as usize,
    };

    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(JWT_SECRET),
    )
    .unwrap()
}

#[actix_web::test]
async fn test_admin_api_valid_credentials() {
    let app = test::init_service(
        App::new()
            .wrap(AuthMiddleware::new(Some(Role::Admin)))
            .configure(admin::configure),
    )
    .await;

    let token = create_test_token(Role::Admin, false);
    let req = test::TestRequest::get()
        .uri("/admin/dashboard")
        .insert_header(("Authorization", format!("Bearer {}", token)))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert!(resp.status().is_success());
    
    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["message"], "Admin dashboard");
    assert_eq!(body["user"], "test_user");
}

#[actix_web::test]
async fn test_admin_api_invalid_credentials() {
    let app = test::init_service(
        App::new()
            .wrap(AuthMiddleware::new(Some(Role::Admin)))
            .configure(admin::configure),
    )
    .await;

    let req = test::TestRequest::get()
        .uri("/admin/dashboard")
        .insert_header(("Authorization", "Bearer invalid_token_here"))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 401);
}

#[actix_web::test]
async fn test_admin_api_missing_credentials() {
    let app = test::init_service(
        App::new()
            .wrap(AuthMiddleware::new(Some(Role::Admin)))
            .configure(admin::configure),
    )
    .await;

    let req = test::TestRequest::get()
        .uri("/admin/dashboard")
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 401);
}

#[actix_web::test]
async fn test_admin_api_expired_token() {
    let app = test::init_service(
        App::new()
            .wrap(AuthMiddleware::new(Some(Role::Admin)))
            .configure(admin::configure),
    )
    .await;

    let token = create_test_token(Role::Admin, true);
    let req = test::TestRequest::get()
        .uri("/admin/dashboard")
        .insert_header(("Authorization", format!("Bearer {}", token)))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 401);
}

#[actix_web::test]
async fn test_admin_api_authorization() {
    let app = test::init_service(
        App::new()
            .wrap(AuthMiddleware::new(Some(Role::Admin)))
            .configure(admin::configure),
    )
    .await;

    // Test with User role (should fail)
    let user_token = create_test_token(Role::User, false);
    let req = test::TestRequest::get()
        .uri("/admin/dashboard")
        .insert_header(("Authorization", format!("Bearer {}", user_token)))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 403);

    // Test with Guest role (should fail)
    let guest_token = create_test_token(Role::Guest, false);
    let req = test::TestRequest::get()
        .uri("/admin/dashboard")
        .insert_header(("Authorization", format!("Bearer {}", guest_token)))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 403);

    // Test with Admin role (should succeed)
    let admin_token = create_test_token(Role::Admin, false);
    let req = test::TestRequest::get()
        .uri("/admin/dashboard")
        .insert_header(("Authorization", format!("Bearer {}", admin_token)))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert!(resp.status().is_success());
}
