use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    
    #[error("Tenant not found")]
    TenantNotFound,
    
    #[error("Invalid API key")]
    InvalidApiKey,
    
    #[error("Unauthorized")]
    Unauthorized,
    
    #[error("Transaction not found")]
    TransactionNotFound,
    
    #[error("Configuration error: {0}")]
    Config(String),
    
    #[error("Internal server error")]
    Internal,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            AppError::TenantNotFound | AppError::InvalidApiKey => {
                (StatusCode::UNAUTHORIZED, self.to_string())
            }
            AppError::Unauthorized => (StatusCode::FORBIDDEN, self.to_string()),
            AppError::TransactionNotFound => (StatusCode::NOT_FOUND, self.to_string()),
            AppError::Database(_) | AppError::Internal => {
                tracing::error!("Internal error: {}", self);
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string())
            }
            AppError::Config(ref msg) => {
                tracing::error!("Config error: {}", msg);
                (StatusCode::INTERNAL_SERVER_ERROR, "Configuration error".to_string())
            }
        };

        (status, Json(json!({ "error": message }))).into_response()
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
