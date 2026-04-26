// Example: Using feature flags to guard functionality
// This file demonstrates how to use feature flags in your handlers

use crate::AppState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde_json::json;

// Example: Conditional logic based on feature flag
pub async fn example_handler(State(state): State<AppState>) -> impl IntoResponse {
    // Check if experimental processor is enabled
    if state
        .feature_flags
        .is_enabled("experimental_processor")
        .await
    {
        // Use experimental logic
        tracing::info!("Using experimental processor");
        (
            StatusCode::OK,
            Json(json!({"processor": "experimental", "status": "active"})),
        )
    } else {
        // Use stable logic
        tracing::info!("Using stable processor");
        (
            StatusCode::OK,
            Json(json!({"processor": "stable", "status": "active"})),
        )
    }
}

// Example: Feature-gated endpoint
pub async fn new_asset_handler(State(state): State<AppState>) -> impl IntoResponse {
    // Check if new asset support is enabled
    if !state.feature_flags.is_enabled("new_asset_support").await {
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({"error": "New asset support is not enabled"})),
        )
            .into_response();
    }

    // Feature is enabled, proceed with logic
    (
        StatusCode::OK,
        Json(json!({"message": "New asset processing available"})),
    )
        .into_response()
}

// Example: Multiple flag checks
pub async fn advanced_handler(State(state): State<AppState>) -> impl IntoResponse {
    let experimental = state
        .feature_flags
        .is_enabled("experimental_processor")
        .await;
    let new_assets = state.feature_flags.is_enabled("new_asset_support").await;

    match (experimental, new_assets) {
        (true, true) => {
            // Both features enabled
            (StatusCode::OK, Json(json!({"mode": "full_experimental"})))
        }
        (true, false) => {
            // Only experimental processor
            (StatusCode::OK, Json(json!({"mode": "experimental_only"})))
        }
        (false, true) => {
            // Only new assets
            (StatusCode::OK, Json(json!({"mode": "new_assets_only"})))
        }
        (false, false) => {
            // Stable mode
            (StatusCode::OK, Json(json!({"mode": "stable"})))
        }
    }
}
