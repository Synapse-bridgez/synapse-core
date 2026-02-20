pub mod config;
pub mod db;
pub mod error;
pub mod handlers;
pub mod services;
pub mod stellar;
pub mod graphql;

use axum::{Router, routing::{get, post}};
use crate::stellar::HorizonClient;
use crate::graphql::schema::{AppSchema, build_schema};

#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::PgPool,
    pub horizon_client: HorizonClient,
}

#[derive(Clone)]
pub struct ApiState {
    pub app_state: AppState,
    pub graphql_schema: AppSchema,
}

pub fn create_app(app_state: AppState) -> Router {
    let graphql_schema = build_schema(app_state.clone());
    let state = ApiState {
        app_state,
        graphql_schema,
    };

    Router::new()
        .route("/health", get(handlers::health))
        .route("/settlements", get(handlers::settlements::list_settlements))
        .route("/settlements/:id", get(handlers::settlements::get_settlement))
        .route("/callback", post(handlers::webhook::callback))
        .route("/transactions/:id", get(handlers::webhook::get_transaction))
        .route("/graphql", post(handlers::graphql::graphql_handler).get(handlers::graphql::subscription_handler))
        .route("/graphql/playground", get(handlers::graphql::graphql_playground))
        .with_state(state)
}
