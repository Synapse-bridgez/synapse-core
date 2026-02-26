use crate::db::pool_manager::PoolManager;
use axum::{extract::State, http::StatusCode, response::IntoResponse};

pub async fn search_transactions(State(_pool_manager): State<PoolManager>) -> impl IntoResponse {
    StatusCode::NOT_IMPLEMENTED
}

/// Wrapper for use with ApiState in create_app
pub async fn search_transactions_wrapper(
    State(api_state): State<crate::ApiState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    // simply call the underlying stub and pack it in Ok
    let _ = search_transactions(State(api_state.app_state.pool_manager)).await;
    Ok(StatusCode::NOT_IMPLEMENTED)
}
