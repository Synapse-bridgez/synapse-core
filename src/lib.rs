use axum::{Router, routing::{get, post}};
use crate::stellar::HorizonClient;
// use crate::graphql::schema::{AppSchema, build_schema}; // Temporarily disabled

#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::PgPool,
    pub horizon_client: HorizonClient,
    pub health_checker: std::sync::Arc<health::HealthChecker>,
}

#[derive(Clone)]
pub struct ApiState {
    pub app_state: AppState,
    pub graphql_schema: (), // Temporarily disabled
}

pub mod config;
pub mod db;
pub mod error;
pub mod handlers;
pub mod health;
pub mod services;
pub mod stellar;
// pub mod graphql; // Temporarily disabled
pub mod utils;
pub mod schemas;

pub fn create_app(app_state: AppState) -> Router {
    Router::new()
        .route("/health", get(handlers::health))
        .route("/settlements", get(handlers::settlements::list_settlements))
        .route("/settlements/:id", get(handlers::settlements::get_settlement))
        .route("/callback", post(handlers::webhook::callback))
        .route("/transactions", get(handlers::webhook::list_transactions_api))
        .route("/transactions/:id", get(handlers::webhook::get_transaction))
        .with_state(app_state)
}
