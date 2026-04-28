// Example: Using feature flags to guard functionality
// This file demonstrates how to use feature flags in your handlers

use crate::AppState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use crate::error::AppError;
use serde_json::json;

// Example: Conditional logic based on feature flag
pub async fn example_handler(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    // Check if experimental processor is enabled
    if state
        .feature_flags
        .is_enabled("experimental_processor")
        .await
    {
        // Use experimental logic
        tracing::info!("Using experimental processor");
        Ok((
            StatusCode::OK,
            Json(json!({"processor": "experimental", "status": "active"})),
        ))
    } else {
        // Use stable logic
        tracing::info!("Using stable processor");
        Ok((
            StatusCode::OK,
            Json(json!({"processor": "stable", "status": "active"})),
        ))
    }
}

// Example: Feature-gated endpoint
pub async fn new_asset_handler(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    // Check if new asset support is enabled
    if !state.feature_flags.is_enabled("new_asset_support").await {
        return Err(AppError::Internal("New asset support is not enabled".to_string()));
    }

    // Feature is enabled, proceed with logic
    Ok((
        StatusCode::OK,
        Json(json!({"message": "New asset processing available"})),
    ))
}

// Example: Multiple flag checks
pub async fn advanced_handler(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    let experimental = state
        .feature_flags
        .is_enabled("experimental_processor")
        .await;
    let new_assets = state.feature_flags.is_enabled("new_asset_support").await;

    match (experimental, new_assets) {
        (true, true) => {
            // Both features enabled
            Ok((StatusCode::OK, Json(json!({"mode": "full_experimental"}))))
        }
        (true, false) => {
            // Only experimental processor
            Ok((StatusCode::OK, Json(json!({"mode": "experimental_only"}))))
        }
        (false, true) => {
            // Only new assets
            Ok((StatusCode::OK, Json(json!({"mode": "new_assets_only"}))))
        }
        (false, false) => {
            // Stable mode
            Ok((StatusCode::OK, Json(json!({"mode": "stable"}))))
        }
    }
}
