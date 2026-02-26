pub mod config;
pub mod db;
pub mod error;
pub mod graphql;
pub mod handlers;
pub mod health;
pub mod metrics;
pub mod middleware;
pub mod readiness;
pub mod schemas;
pub mod secrets;
pub mod services;
pub mod startup;
pub mod stellar;
pub mod utils;
pub mod validation;

use crate::db::pool_manager::PoolManager;
use crate::graphql::schema::AppSchema;
use crate::handlers::ws::TransactionStatusUpdate;
pub use crate::readiness::ReadinessState;
use crate::services::feature_flags::FeatureFlagService;
use crate::services::query_cache::QueryCache;
use crate::stellar::HorizonClient;
use axum::{
    middleware as axum_middleware,
    routing::{get, post},
    Router,
};
use tokio::sync::broadcast;

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
    pub query_cache: QueryCache,
}

#[derive(Clone)]
pub struct ApiState {
    pub app_state: AppState,
    pub graphql_schema: AppSchema,
}

pub fn create_app(app_state: AppState) -> Router {
    let graphql_schema = crate::graphql::schema::build_schema(app_state.clone());
    let api_state = ApiState {
        app_state,
        graphql_schema,
    };

    // Callback routes with validation middleware
    let callback_routes = Router::new()
        .route("/callback", post(handlers::webhook::callback))
        .route("/callback/transaction", post(handlers::webhook::callback))
        .layer(axum_middleware::from_fn(
            crate::middleware::validate::validate_callback,
        ));

    // Webhook route with validation middleware
    let webhook_routes = Router::new()
        .route("/webhook", post(handlers::webhook::handle_webhook))
        .layer(axum_middleware::from_fn(
            crate::middleware::validate::validate_webhook,
        ));

    Router::new()
        .route("/health", get(handlers::health))
        .route("/ready", get(handlers::ready))
        .route("/errors", get(handlers::error_catalog))
        .route("/settlements", get(handlers::settlements::list_settlements))
        .route(
            "/settlements/:id",
            get(handlers::settlements::get_settlement),
        )
        .merge(callback_routes)
        .merge(webhook_routes)
        .route("/transactions/:id", get(handlers::webhook::get_transaction))
        .route("/graphql", post(handlers::graphql::graphql_handler))
        .route("/export", get(handlers::export::export_transactions))
        .route("/stats/status", get(handlers::stats::status_counts))
        .route("/stats/daily", get(handlers::stats::daily_totals))
        .route("/stats/assets", get(handlers::stats::asset_stats))
        .route("/cache/metrics", get(handlers::stats::cache_metrics))
        .with_state(api_state)
}
