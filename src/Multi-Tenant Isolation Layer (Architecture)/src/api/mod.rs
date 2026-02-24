mod handlers;

use axum::{
    routing::{get, post, delete, put},
    Router,
};

use crate::config::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/transactions", post(handlers::create_transaction))
        .route("/transactions", get(handlers::list_transactions))
        .route("/transactions/:id", get(handlers::get_transaction))
        .route("/transactions/:id", put(handlers::update_transaction))
        .route("/transactions/:id", delete(handlers::delete_transaction))
        .route("/webhook", post(handlers::webhook_handler))
}
