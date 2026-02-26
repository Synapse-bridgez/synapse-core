use crate::db::queries::{AssetStats, DailyTotal, StatusCount};
use crate::services::query_cache::{
    cache_key_asset_stats, cache_key_daily_totals, cache_key_status_counts, CacheConfig,
};
use crate::ApiState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;
use std::time::Duration;

#[derive(Deserialize)]
pub struct DailyTotalsQuery {
    #[serde(default = "default_days")]
    days: i32,
}

fn default_days() -> i32 {
    7
}

pub async fn status_counts(State(state): State<ApiState>) -> impl IntoResponse {
    let cache_key = cache_key_status_counts();
    let config = CacheConfig::default();

    // Try cache first
    if let Ok(Some(cached)) = state
        .app_state
        .query_cache
        .get::<Vec<StatusCount>>(&cache_key)
        .await
    {
        return (StatusCode::OK, Json(cached));
    }

    // Cache miss - query database
    match crate::db::queries::get_status_counts(&state.app_state.db).await {
        Ok(counts) => {
            // Store in cache
            let _ = state
                .app_state
                .query_cache
                .set(
                    &cache_key,
                    &counts,
                    Duration::from_secs(config.status_counts_ttl),
                )
                .await;

            (StatusCode::OK, Json(counts))
        }
        Err(e) => {
            tracing::error!("Failed to get status counts: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Vec::<StatusCount>::new()),
            )
        }
    }
}

pub async fn daily_totals(
    State(state): State<ApiState>,
    axum::extract::Query(query): axum::extract::Query<DailyTotalsQuery>,
) -> impl IntoResponse {
    let cache_key = cache_key_daily_totals(query.days);
    let config = CacheConfig::default();

    // Try cache first
    if let Ok(Some(cached)) = state
        .app_state
        .query_cache
        .get::<Vec<DailyTotal>>(&cache_key)
        .await
    {
        return (StatusCode::OK, Json(cached));
    }

    // Cache miss - query database
    match crate::db::queries::get_daily_totals(&state.app_state.db, query.days).await {
        Ok(totals) => {
            // Store in cache
            let _ = state
                .app_state
                .query_cache
                .set(
                    &cache_key,
                    &totals,
                    Duration::from_secs(config.daily_totals_ttl),
                )
                .await;

            (StatusCode::OK, Json(totals))
        }
        Err(e) => {
            tracing::error!("Failed to get daily totals: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Vec::<DailyTotal>::new()),
            )
        }
    }
}

pub async fn asset_stats(State(state): State<ApiState>) -> impl IntoResponse {
    let cache_key = cache_key_asset_stats();
    let config = CacheConfig::default();

    // Try cache first
    if let Ok(Some(cached)) = state
        .app_state
        .query_cache
        .get::<Vec<AssetStats>>(&cache_key)
        .await
    {
        return (StatusCode::OK, Json(cached));
    }

    // Cache miss - query database
    match crate::db::queries::get_asset_stats(&state.app_state.db).await {
        Ok(stats) => {
            // Store in cache
            let _ = state
                .app_state
                .query_cache
                .set(
                    &cache_key,
                    &stats,
                    Duration::from_secs(config.asset_stats_ttl),
                )
                .await;

            (StatusCode::OK, Json(stats))
        }
        Err(e) => {
            tracing::error!("Failed to get asset stats: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Vec::<AssetStats>::new()),
            )
        }
    }
}

pub async fn cache_metrics(State(state): State<ApiState>) -> impl IntoResponse {
    let metrics = state.app_state.query_cache.metrics();
    (StatusCode::OK, Json(metrics))
}
