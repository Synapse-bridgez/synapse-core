use crate::db::queries::{AssetStats, DailyTotal, StatusCount};
use crate::services::query_cache::{
    cache_key_asset_stats, cache_key_daily_totals, cache_key_status_counts, CacheConfig,
};
use crate::ApiState;
use axum::{
    extract::State,
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
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

#[derive(Debug, serde::Serialize)]
pub struct CombinedCacheMetrics {
    pub query_cache: crate::services::query_cache::CacheMetrics,
    pub idempotency_cache_hits: u64,
    pub idempotency_cache_misses: u64,
    pub idempotency_lock_acquired: u64,
    pub idempotency_lock_contention: u64,
    pub idempotency_errors: u64,
    pub idempotency_fallback_count: u64,
}

pub async fn status_counts(State(state): State<ApiState>) -> impl IntoResponse {
    let cache_key = cache_key_status_counts();
    let config = CacheConfig::default();

    if let Ok(Some(cached)) = state
        .app_state
        .query_cache
        .get::<Vec<StatusCount>>(&cache_key)
        .await
    {
        return (StatusCode::OK, Json(cached)).into_response();
    }

    let (pool, replica_used) = state.app_state.pool_manager.read_pool().await;
    match crate::db::queries::get_status_counts(pool).await {
        Ok(counts) => {
            let _ = state
                .app_state
                .query_cache
                .set(
                    &cache_key,
                    &counts,
                    Duration::from_secs(config.status_counts_ttl),
                )
                .await;

            let mut response: Response = (StatusCode::OK, Json(counts)).into_response();
            if replica_used {
                response
                    .headers_mut()
                    .insert("X-Read-Consistency", HeaderValue::from_static("eventual"));
            }
            response
        }
        Err(e) => {
            tracing::error!("Failed to get status counts: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Vec::<StatusCount>::new()),
            )
                .into_response()
        }
    }
}

pub async fn daily_totals(
    State(state): State<ApiState>,
    axum::extract::Query(query): axum::extract::Query<DailyTotalsQuery>,
) -> impl IntoResponse {
    let cache_key = cache_key_daily_totals(query.days);
    let config = CacheConfig::default();

    if let Ok(Some(cached)) = state
        .app_state
        .query_cache
        .get::<Vec<DailyTotal>>(&cache_key)
        .await
    {
        return (StatusCode::OK, Json(cached)).into_response();
    }

    let (pool, replica_used) = state.app_state.pool_manager.read_pool().await;
    match crate::db::queries::get_daily_totals(pool, query.days).await {
        Ok(totals) => {
            let _ = state
                .app_state
                .query_cache
                .set(
                    &cache_key,
                    &totals,
                    Duration::from_secs(config.daily_totals_ttl),
                )
                .await;

            let mut response: Response = (StatusCode::OK, Json(totals)).into_response();
            if replica_used {
                response
                    .headers_mut()
                    .insert("X-Read-Consistency", HeaderValue::from_static("eventual"));
            }
            response
        }
        Err(e) => {
            tracing::error!("Failed to get daily totals: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Vec::<DailyTotal>::new()),
            )
                .into_response()
        }
    }
}

pub async fn asset_stats(State(state): State<ApiState>) -> impl IntoResponse {
    let cache_key = cache_key_asset_stats();
    let config = CacheConfig::default();

    if let Ok(Some(cached)) = state
        .app_state
        .query_cache
        .get::<Vec<AssetStats>>(&cache_key)
        .await
    {
        return (StatusCode::OK, Json(cached)).into_response();
    }

    let (pool, replica_used) = state.app_state.pool_manager.read_pool().await;
    match crate::db::queries::get_asset_stats(pool).await {
        Ok(stats) => {
            let _ = state
                .app_state
                .query_cache
                .set(
                    &cache_key,
                    &stats,
                    Duration::from_secs(config.asset_stats_ttl),
                )
                .await;

            let mut response: Response = (StatusCode::OK, Json(stats)).into_response();
            if replica_used {
                response
                    .headers_mut()
                    .insert("X-Read-Consistency", HeaderValue::from_static("eventual"));
            }
            response
        }
        Err(e) => {
            tracing::error!("Failed to get asset stats: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(Vec::<AssetStats>::new()),
            )
                .into_response()
        }
    }
}

pub async fn cache_metrics(State(state): State<ApiState>) -> impl IntoResponse {
    let query_cache_metrics = state.app_state.query_cache.metrics();
    let combined_metrics = CombinedCacheMetrics {
        query_cache: query_cache_metrics,
        idempotency_cache_hits: 0,
        idempotency_cache_misses: 0,
        idempotency_lock_acquired: 0,
        idempotency_lock_contention: 0,
        idempotency_errors: 0,
        idempotency_fallback_count: 0,
    };
    (StatusCode::OK, Json(combined_metrics))
}
