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
pub mod readiness;

use axum::{Router, routing::{get, post}};
use crate::stellar::HorizonClient;
use crate::services::feature_flags::FeatureFlagService;
use crate::db::pool_manager::PoolManager;
use tokio::sync::broadcast;
use crate::handlers::ws::TransactionStatusUpdate;
pub use crate::readiness::ReadinessState;
// use crate::graphql::schema::{AppSchema, build_schema};  // Temporarily commented out to resolve compilation issues

#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::PgPool,
    pub pool_manager: PoolManager,
    pub horizon_client: HorizonClient,
    pub feature_flags: FeatureFlagService,
    pub redis_url: String,
    pub start_time: std::time::Instant,
    pub readiness: ReadinessState,
    pub tx_broadcast: broadcast::Sender<TransactionStatusUpdate>,
}

#[derive(Clone)]
pub struct ApiState {
    pub app_state: AppState,
    // pub graphql_schema: AppSchema,  // Temporarily commented out to resolve compilation issues
}

pub fn create_app(app_state: AppState) -> Router {
    let api_state = ApiState {
        app_state: app_state.clone(),
    };
    
    Router::new()
        .route("/health", get(handlers::health))
        .route("/ready", get(handlers::ready))
        .route("/errors", get(handlers::error_catalog))
        .route("/settlements", get(handlers::settlements::list_settlements))
        .route("/settlements/:id", get(handlers::settlements::get_settlement))
        .route("/callback", post(handlers::webhook::callback))
        .route("/callback/transaction", post(handlers::webhook::callback)) // Backward compatibility
        .route("/transactions/:id", get(handlers::webhook::get_transaction))
        .route("/transactions/search", get(handlers::search::search_transactions_wrapper))
        .route("/ws", get(handlers::ws::ws_handler))
        // .route("/graphql", post(handlers::graphql::graphql_handler).get(handlers::graphql::subscription_handler))
        // .route("/graphql/playground", get(handlers::graphql::graphql_playground))
        .with_state(api_state)
}
