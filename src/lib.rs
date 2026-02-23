pub mod config;
pub mod db;
pub mod error;
pub mod handlers;
pub mod services;
pub mod stellar;
pub mod graphql;
pub mod schemas;
pub mod middleware;
pub mod startup;
pub mod utils;
pub mod health;
pub mod metrics;
pub mod validation;

use axum::{Router, routing::{get, post}};
use crate::stellar::HorizonClient;
use crate::services::feature_flags::FeatureFlagService;
use crate::db::pool_manager::PoolManager;
// use crate::graphql::schema::{AppSchema, build_schema};  // Temporarily commented out to resolve compilation issues

#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::PgPool,
    pub pool_manager: PoolManager,
    pub horizon_client: HorizonClient,
    pub feature_flags: FeatureFlagService,
    pub redis_url: String,
    pub start_time: std::time::Instant,
}

#[derive(Clone)]
pub struct ApiState {
    pub app_state: AppState,
    // pub graphql_schema: AppSchema,  // Temporarily commented out to resolve compilation issues
}

pub fn create_app(app_state: AppState) -> Router {
    let api_state = ApiState {
        app_state,
    };
    
    Router::new()
        .route("/health", get(handlers::health))
        .route("/settlements", get(handlers::settlements::list_settlements))
        .route("/settlements/:id", get(handlers::settlements::get_settlement))
        .route("/callback", post(handlers::webhook::callback))
        .route("/transactions", get(handlers::webhook::list_transactions_api))
        .route("/transactions/:id", get(handlers::webhook::get_transaction))
        // .route("/graphql", post(handlers::graphql::graphql_handler).get(handlers::graphql::subscription_handler))
        // .route("/graphql/playground", get(handlers::graphql::graphql_playground))
        .with_state(api_state)
}
