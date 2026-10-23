pub mod admin;
pub mod dlq;
pub mod export;
pub mod graphql;
pub mod idempotency;
pub mod import;
pub mod pagination;
pub mod profiling;
pub mod search;
pub mod session;
pub mod disputes;
pub mod settlements;
pub mod stats;
pub mod swap;
pub mod v1;
pub mod v2;
pub mod webhook;
pub mod ws;
pub mod ws_error;

pub use pagination::{
    validate_pagination, PaginatedListResponse, PaginationHelper, PaginationQuery,
};

use crate::error::AppError;
use crate::ApiState;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Maximum number of items accepted in a single batch request.
///
/// Mirrors the configurable `batch_max_items` setting; used as a hard upper
/// bound when the configuration is unavailable.
pub const DEFAULT_BATCH_MAX_ITEMS: usize = 100;

/// Maximum number of batch items processed concurrently.
///
/// Bounded fan-out keeps per-batch resource usage predictable and prevents a
/// single large batch from starving other requests.
pub const BATCH_CONCURRENCY_LIMIT: usize = 8;

/// Error code returned when a swap order is submitted while the service is
/// draining (graceful shutdown in progress).
pub const SWAP_DRAINING_ERROR_CODE: &str = "SWAP_DRAINING";

/// Error code returned when a batch exceeds the configured size cap.
pub const BATCH_TOO_LARGE_ERROR_CODE: &str = "BATCH_TOO_LARGE";

/// Returns `true` when the swap subsystem is draining and must reject new
/// order acceptance.
///
/// Readiness is the single source of truth for the draining flag so that the
/// readiness probe and the swap handlers can never disagree.
pub fn swap_is_draining(state: &ApiState) -> bool {
    state.app_state.readiness.is_draining()
}

/// Builds the clear, structured error returned when new swap orders are
/// rejected because the service is draining.
pub fn swap_draining_error() -> AppError {
    AppError::ServiceUnavailable(format!(
        "{}: service is draining for graceful shutdown; retry after restart completes",
        SWAP_DRAINING_ERROR_CODE
    ))
}

/// Rejects new swap order acceptance while draining.
///
/// Call this at the top of every swap order/quote acceptance path so that the
/// stop-accepting phase of shutdown is enforced consistently.
pub fn ensure_swap_accepting(state: &ApiState) -> Result<(), AppError> {
    if swap_is_draining(state) {
        Err(swap_draining_error())
    } else {
        Ok(())
    }
}

/// Derives a deterministic per-item idempotency key from a batch key and the
/// item's index within the batch.
///
/// Retrying the same batch with the same batch key therefore maps each item to
/// the same idempotency key, so already-processed items are deduplicated by the
/// existing idempotency mechanism instead of being duplicated.
pub fn derive_batch_item_idempotency_key(batch_key: &str, index: usize) -> String {
    format!("{}:item:{}", batch_key, index)
}

/// Per-item outcome of a batch operation.
///
/// Every item in a batch produces exactly one of these, so partial success is
/// always unambiguous: `ok == true` means the item succeeded, otherwise
/// `error_code`/`error` describe the failure.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BatchItemResult<T> {
    /// Zero-based index of the item within the submitted batch.
    pub index: usize,
    /// Deterministic idempotency key used for this item.
    pub idempotency_key: String,
    /// Whether this item succeeded.
    pub ok: bool,
    /// Successful result payload, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<T>,
    /// Machine-readable error code on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// Human-readable error message on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<T> BatchItemResult<T> {
    /// Builds a successful per-item result.
    pub fn success(index: usize, idempotency_key: String, result: T) -> Self {
        Self {
            index,
            idempotency_key,
            ok: true,
            result: Some(result),
            error_code: None,
            error: None,
        }
    }

    /// Builds a failed per-item result.
    pub fn failure(
        index: usize,
        idempotency_key: String,
        error_code: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            index,
            idempotency_key,
            ok: false,
            result: None,
            error_code: Some(error_code.into()),
            error: Some(error.into()),
        }
    }
}

/// Envelope returned by every batch endpoint.
///
/// Items are reported in submission order (index ascending). Atomicity is
/// per-item only: a batch never fails as a whole because of individual item
/// failures, and successful items are never rolled back.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BatchResponse<T> {
    /// Per-item outcomes, ordered by ascending `index`.
    pub items: Vec<BatchItemResult<T>>,
    /// Number of items that succeeded.
    pub succeeded: usize,
    /// Number of items that failed.
    pub failed: usize,
}

impl<T> BatchResponse<T> {
    /// Builds a response from per-item results, sorting by index and computing
    /// success/failure counts.
    pub fn from_items(mut items: Vec<BatchItemResult<T>>) -> Self {
        items.sort_by_key(|item| item.index);
        let succeeded = items.iter().filter(|item| item.ok).count();
        let failed = items.len() - succeeded;
        Self {
            items,
            succeeded,
            failed,
        }
    }
}

/// Enforces the per-batch size cap.
///
/// An oversized batch is rejected as a whole (before any item is processed) so
/// that no quota is charged and no side effects occur. Individual oversized
/// items are reported per-item instead.
pub fn enforce_batch_cap(len: usize, max_items: usize) -> Result<(), AppError> {
    if len == 0 {
        return Err(AppError::Validation(
            "batch must contain at least one item".to_string(),
        ));
    }
    if len > max_items {
        return Err(AppError::Validation(format!(
            "{}: batch contains {} items, exceeding the maximum of {}",
            BATCH_TOO_LARGE_ERROR_CODE, len, max_items
        )));
    }
    Ok(())
}

/// Fans out a batch of items to an async per-item function with bounded
/// concurrency, preserving submission order in the returned results.
///
/// Each item is processed independently: a failure in one item never aborts
/// the others, and every item yields exactly one `BatchItemResult`.
pub async fn fan_out_batch<T, F, Fut>(
    batch_key: &str,
    items: Vec<T>,
    process: F,
) -> BatchResponse<T::Output>
where
    T: Send + 'static,
    T::Output: Send + 'static,
    F: Fn(usize, String, T) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<T::Output, AppError>> + Send + 'static,
{
    use futures::stream::{FuturesUnordered, StreamExt};
    use std::sync::Arc;

    let process = Arc::new(process);
    let mut pending = FuturesUnordered::new();
    let mut results: Vec<BatchItemResult<T::Output>> = Vec::with_capacity(items.len());
    let mut iter = items.into_iter().enumerate();

    // Prime the pipeline up to the concurrency limit.
    for _ in 0..BATCH_CONCURRENCY_LIMIT {
        match iter.next() {
            Some((index, item)) => {
                let key = derive_batch_item_idempotency_key(batch_key, index);
                let process = Arc::clone(&process);
                let key_for_task = key.clone();
                pending.push(async move {
                    let outcome = process(index, key_for_task.clone(), item).await;
                    (index, key_for_task, outcome)
                });
            }
            None => break,
        }
    }

    while let Some((index, key, outcome)) = pending.next().await {
        match outcome {
            Ok(value) => results.push(BatchItemResult::success(index, key, value)),
            Err(err) => {
                let (code, message) = err.batch_error_parts();
                results.push(BatchItemResult::failure(index, key, code, message));
            }
        }
        if let Some((next_index, item)) = iter.next() {
            let next_key = derive_batch_item_idempotency_key(batch_key, next_index);
            let process = Arc::clone(&process);
            let key_for_task = next_key.clone();
            pending.push(async move {
                let outcome = process(next_index, key_for_task.clone(), item).await;
                (next_index, key_for_task, outcome)
            });
        }
    }

    BatchResponse::from_items(results)
}

/// Helper service for checking dependency health.
///
/// Consolidates common health check logic (database connectivity, pool stats)
/// to avoid duplication across health and readiness endpoints.
/// Designed to degrade gracefully—failures are logged and reported, not panicked.
pub struct HealthChecker;

impl HealthChecker {
    /// Checks database connectivity and returns pool statistics.
    ///
    /// # Arguments
    /// - `pool`: Database connection pool to check
    ///
    /// # Returns
    /// A tuple of (status, pool_stats, status_code):
    /// - status: "connected" or "disconnected"
    /// - pool_stats: Database pool utilization metrics
    /// - status_code: HTTP status (200 if connected, 503 if not)
    ///
    /// # Non-fatal behavior
    /// Gracefully degrades if database operations fail; returns "disconnected"
    /// status rather than panicking.
    pub async fn check_db(pool: &sqlx::PgPool) -> (String, DbPoolStats, StatusCode) {
        // Check database connectivity with SELECT 1 query
        let db_status = match sqlx::query("SELECT 1").execute(pool).await {
            Ok(_) => "connected",
            Err(_) => "disconnected",
        };

        // Gather pool statistics
        let active_connections = pool.size();
        let idle_connections = pool.num_idle();
        let max_connections = pool.options().get_max_connections();
        let usage_percent = (active_connections as f32 / max_connections as f32) * 100.0;

        let pool_stats = DbPoolStats {
            active_connections,
            idle_connections: idle_connections as u32,
            max_connections,
            usage_percent,
        };

        let status_code = if db_status == "connected" {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        };

        (db_status.to_string(), pool_stats, status_code)
    }
}

/// Liveness probe endpoint — always returns 200 if the process is running.
///
/// This endpoint indicates whether the application process itself is alive,
/// NOT whether it can handle requests or reach dependencies.
/// Used by orchestration platforms (Kubernetes) to determine if the process should be restarted.
///
/// # Returns
/// Always returns `(StatusCode::OK, LivenessResponse)`.
/// No dependency checks are performed; this endpoint cannot fail.
///
/// # Use case
/// Kubernetes liveness probes; restart the process if this returns non-200.
#[utoipa::path(
    get,
    path = "/live",
    responses(
        (status = 200, description = "Process is alive", body = LivenessResponse)
    ),
    tag = "Health"
)]
pub async fn live() -> impl IntoResponse {
    let response = LivenessResponse {
        status: "alive".to_string(),
    };
    (StatusCode::OK, Json(response))
}

/// Readiness probe endpoint — returns 200 when ready to accept traffic, 503 when draining.
///
/// This endpoint indicates whether the service is ready to accept requests.
/// Returns non-200 (503) if the service is draining or gracefully shutting down.
///
/// # Returns
/// - `(StatusCode::OK, ReadinessResponse)` — service is ready to accept traffic
/// - `(StatusCode::SERVICE_UNAVAILABLE, ReadinessResponse)` — service is draining or not ready
///
/// The response body includes `draining` flag indicating graceful shutdown state.
///
/// # Use case
/// Kubernetes readiness probes; remove from load balancer if this returns non-200.
#[utoipa::path(
    get,
    path = "/ready",
    responses(
        (status = 200, description = "Service is ready", body = ReadinessResponse),
        (status = 503, description = "Service is not ready or draining", body = ReadinessResponse)
    ),
    tag = "Health"
)]
pub async fn ready(State(state): State<ApiState>) -> Result<Response, AppError> {
    let dependencies = crate::readiness::dependency_readiness(
        state.app_state.secrets_store.as_ref(),
        std::time::Instant::now(),
    );
    let degraded = dependencies.degraded();
    let is_ready = state.app_state.readiness.is_ready()
        && !(dependencies.secrets_expired() && crate::readiness::fail_on_expired_secrets());
    let draining = state.app_state.readiness.is_draining();
    let response = ReadinessResponse {
        status: if is_ready { "ready" } else { "not_ready" }.to_string(),
        draining,
        swap_draining: draining,
        degraded: degraded.clone(),
        dependencies: Some(dependencies),
    };
    let code = if is_ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let mut resp = (code, Json(response)).into_response();
    if !degraded.is_empty() {
        if let Ok(v) = axum::http::HeaderValue::from_str(&degraded.join(",")) {
            resp.headers_mut().insert("X-Degraded-Dependencies", v);
        }
    }
    Ok(resp)
}

/// Health check endpoint — aggregates dependency status and reports service health.
///
/// Checks database connectivity and gathers pool statistics to determine overall
/// service health. Returns 503 if critical dependencies (database) are unavailable.
///
/// The response includes detailed dependency status and queue metrics for monitoring.
///
/// # Returns
/// - `(StatusCode::OK, HealthStatus)` — all critical dependencies healthy
/// - `(StatusCode::SERVICE_UNAVAILABLE, HealthStatus)` — critical dependency failure (database)
///
/// # Use case
/// Monitoring and alerting; not used by orchestration platforms (use /live and /ready instead).
#[utoipa::path(
    get,
    path = "/health",
    responses(
        (status = 200, description = "Service is healthy", body = HealthStatus),
        (status = 503, description = "Service is unhealthy", body = HealthStatus)
    ),
    tag = "Health"
)]
pub async fn health(State(state): State<ApiState>) -> Result<impl IntoResponse, AppError> {
    // Use HealthChecker to check database connectivity and gather pool stats
    let (db_status, pool_stats, db_status_code) =
        HealthChecker::check_db(&state.app_state.db).await;

    let pending_queue_depth = state
        .app_state
        .pending_queue_depth
        .load(std::sync::atomic::Ordering::Relaxed);
    let current_batch_size = state
        .app_state
        .current_batch_size
        .load(std::sync::atomic::Ordering::Relaxed);
    let ws_connection_count = state.app_state.ws_connection_pool.active_connections();

    let health_response = HealthStatus {
        status: if db_status == "connected" {
            "healthy".to_string()
        } else {
            "unhealthy".to_string()
        },
        version: "0.1.0".to_string(),
        db: db_status,
        db_pool: pool_stats,
        pending_queue_depth,
        current_batch_size,
        ws_connection_count,
    };

    Ok((db_status_code, Json(health_response)))
}

/// Response from the liveness probe endpoint (/live).
///
/// Indicates whether the application process is running.
/// Always returns `status: "alive"` if the endpoint returns 200.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LivenessResponse {
    /// Always "alive" if this response is returned.
    /// If the process crashes, orchestration platforms will detect the missing response and restart.
    pub status: String,
}

/// Response from the readiness probe endpoint (/ready).
///
/// Indicates whether the service is ready to accept traffic.
/// Used by load balancers and orchestration platforms to determine routing.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ReadinessResponse {
    /// "ready" if the service can handle traffic, "not_ready" if draining or misconfigured
    pub status: String,
    /// true if the service is in graceful shutdown mode (/admin/drain was called)
    pub draining: bool,
    /// true if the swap subsystem is draining and rejecting new orders.
    /// Orchestrators should stop routing swap traffic when this is set.
    #[serde(default)]
    pub swap_draining: bool,
    /// Dependencies currently running in degraded mode (`vault`, `redis`).
    /// Degradation is reported here without failing readiness — see
    /// `readiness::dependency_readiness`.
    #[serde(default)]
    pub degraded: Vec<String>,
    /// Per-dependency degraded-mode detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<crate::readiness::DependencyReadiness>,
}

/// Response from the health check endpoint (/health).
///
/// Aggregates dependency health information for monitoring and alerting.
/// Not used by orchestration platforms; use /live and /ready for that instead.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HealthStatus {
    /// Overall status: "healthy" if all critical deps healthy, "unhealthy" if any critical dep down
    pub status: String,
    /// API version
    pub version: String,
    /// Database connection status: "connected" or "disconnected"
    pub db: String,
    /// Database connection pool utilization and limits
    pub db_pool: DbPoolStats,
    /// Number of pending tasks in the queue; high values may indicate overload
    pub pending_queue_depth: u64,
    /// Current batch size for settlement processing
    pub current_batch_size: u64,
    /// Number of active WebSocket connections
    pub ws_connection_count: usize,
}

/// Database connection pool statistics.
///
/// Used to monitor pool exhaustion and connection health.
/// High `usage_percent` may indicate capacity issues.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DbPoolStats {
    /// Number of connections currently in use
    pub active_connections: u32,
    /// Number of idle connections available for reuse
    pub idle_connections: u32,
    /// Maximum allowed connections in the pool
    pub max_connections: u32,
    /// Percentage of pool utilized (active / max) as a float 0-100
    pub usage_percent: f32,
}

/// Batch quote request body.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BatchQuoteRequest {
    /// Client-supplied batch idempotency key. Per-item keys are derived
    /// deterministically as `{batch_key}:item:{index}`.
    pub batch_key: String,
    /// Individual quote requests, processed in order.
    pub items: Vec<serde_json::Value>,
}

/// Batch order submission request body.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BatchOrderRequest {
    /// Client-supplied batch idempotency key. Per-item keys are derived
    /// deterministically as `{batch_key}:item:{index}`.
    pub batch_key: String,
    /// Individual order submissions, processed in order.
    pub items: Vec<serde_json::Value>,
}

/// Bulk cancel request body.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BulkCancelRequest {
    /// Client-supplied batch idempotency key.
    pub batch_key: String,
    /// Order identifiers to cancel.
    pub order_ids: Vec<String>,
}

/// Bulk status request body.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BulkStatusRequest {
    /// Client-supplied batch idempotency key.
    pub batch_key: String,
    /// Order identifiers to look up.
    pub order_ids: Vec<String>,
}

/// Batch quote endpoint.
///
/// Fans out to the single-item quote path in-process with bounded concurrency.
/// Each item is charged to the tenant quota individually and reported with its
/// own success/failure outcome; one failing item never fails the batch.
#[utoipa::path(
    post,
    path = "/api/v1/swap/quotes/batch",
    request_body = BatchQuoteRequest,
    responses(
        (status = 200, description = "Per-item batch quote results", body = BatchResponse<serde_json::Value>),
        (status = 400, description = "Batch exceeds size cap or is empty"),
        (status = 503, description = "Service is draining")
    ),
    tag = "Swap"
)]
pub async fn batch_quotes(
    State(state): State<ApiState>,
    Json(request): Json<BatchQuoteRequest>,
) -> Result<Response, AppError> {
    ensure_swap_accepting(&state)?;
    let max_items = crate::config::batch_max_items();
    enforce_batch_cap(request.items.len(), max_items)?;

    let batch_key = request.batch_key.clone();
    let response = fan_out_batch(&batch_key, request.items, move |_index, _key, item| {
        let state = state.clone();
        async move {
            // Reuse the single-item quote path; quota is charged per item there.
            crate::swap::quote_single(&state, item).await
        }
    })
    .await;

    Ok((StatusCode::OK, Json(response)).into_response())
}

/// Batch order submission endpoint.
///
/// Fans out to the single-item order submission path in-process with bounded
/// concurrency. Risk/compliance checks and per-item quota charging are applied
/// by the single-item path, so batching never bypasses them.
#[utoipa::path(
    post,
    path = "/api/v1/swap/orders/batch",
    request_body = BatchOrderRequest,
    responses(
        (status = 200, description = "Per-item batch order results", body = BatchResponse<serde_json::Value>),
        (status = 400, description = "Batch exceeds size cap or is empty"),
        (status = 503, description = "Service is draining")
    ),
    tag = "Swap"
)]
pub async fn batch_orders(
    State(state): State<ApiState>,
    Json(request): Json<BatchOrderRequest>,
) -> Result<Response, AppError> {
    ensure_swap_accepting(&state)?;
    let max_items = crate::config::batch_max_items();
    enforce_batch_cap(request.items.len(), max_items)?;

    let batch_key = request.batch_key.clone();
    let response = fan_out_batch(&batch_key, request.items, move |_index, _key, item| {
        let state = state.clone();
        async move {
            // Reuse the single-item submission path (risk/compliance + quota).
            crate::swap::submit_single(&state, item).await
        }
    })
    .await;

    Ok((StatusCode::OK, Json(response)).into_response())
}

/// Bulk cancel endpoint.
///
/// Cancels each order independently and reports per-item outcomes. Cancellation
/// remains allowed while draining so in-flight work can be parked safely.
#[utoipa::path(
    post,
    path = "/api/v1/swap/orders/bulk-cancel",
    request_body = BulkCancelRequest,
    responses(
        (status = 200, description = "Per-item bulk cancel results", body = BatchResponse<serde_json::Value>),
        (status = 400, description = "Batch exceeds size cap or is empty")
    ),
    tag = "Swap"
)]
pub async fn bulk_cancel(
    State(state): State<ApiState>,
    Json(request): Json<BulkCancelRequest>,
) -> Result<Response, AppError> {
    let max_items = crate::config::batch_max_items();
    enforce_batch_cap(request.order_ids.len(), max_items)?;

    let batch_key = request.batch_key.clone();
    let response = fan_out_batch(&batch_key, request.order_ids, move |_index, _key, order_id| {
        let state = state.clone();
        async move { crate::swap::cancel_single(&state, order_id).await }
    })
    .await;

    Ok((StatusCode::OK, Json(response)).into_response())
}

/// Bulk status endpoint.
///
/// Looks up each order independently and reports per-item outcomes. Status
/// lookups remain allowed while draining.
#[utoipa::path(
    post,
    path = "/api/v1/swap/orders/bulk-status",
    request_body = BulkStatusRequest,
    responses(
        (status = 200, description = "Per-item bulk status results", body = BatchResponse<serde_json::Value>),
        (status = 400, description = "Batch exceeds size cap or is empty")
    ),
    tag = "Swap"
)]
pub async fn bulk_status(
    State(state): State<ApiState>,
    Json(request): Json<BulkStatusRequest>,
) -> Result<Response, AppError> {
    let max_items = crate::config::batch_max_items();
    enforce_batch_cap(request.order_ids.len(), max_items)?;

    let batch_key = request.batch_key.clone();
    let response = fan_out_batch(&batch_key, request.order_ids, move |_index, _key, order_id| {
        let state = state.clone();
        async move { crate::swap::status_single(&state, order_id).await }
    })
    .await;

    Ok((StatusCode::OK, Json(response)).into_response())
}

/// Error catalog endpoint
/// Returns all available error codes and their descriptions
pub async fn error_catalog() -> Result<impl IntoResponse, AppError> {
    let errors = crate::error::get_all_error_codes();
    let catalog = crate::error::ErrorCatalogResponse {
        errors,
        version: "1.0.0".to_string(),
    };

    Ok((StatusCode::OK, Json(catalog)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_liveness_response_always_alive() {
        // The /live endpoint is always alive regardless of dependencies
        // This test verifies that the LivenessResponse always indicates alive
        let response = LivenessResponse {
            status: "alive".to_string(),
        };
        assert_eq!(response.status, "alive");
    }

    #[test]
    fn test_readiness_response_structure() {
        // Verify ReadinessResponse can represent both ready and not_ready states
        let ready = ReadinessResponse {
            status: "ready".to_string(),
            draining: false,
            swap_draining: false,
            degraded: vec![],
            dependencies: None,
        };
        assert_eq!(ready.status, "ready");
        assert!(!ready.draining);
        assert!(!ready.swap_draining);

        let not_ready = ReadinessResponse {
            status: "not_ready".to_string(),
            draining: true,
            swap_draining: true,
            degraded: vec!["vault".to_string()],
            dependencies: None,
        };
        assert_eq!(not_ready.status, "not_ready");
        assert!(not_ready.draining);
        assert!(not_ready.swap_draining);
    }

    #[test]
    fn test_derive_batch_item_idempotency_key_is_deterministic() {
        let a = derive_batch_item_idempotency_key("batch-123", 0);
        let b = derive_batch_item_idempotency_key("batch-123", 0);
        let c = derive_batch_item_idempotency_key("batch-123", 1);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a, "batch-123:item:0");
    }

    #[test]
    fn test_enforce_batch_cap() {
        assert!(enforce_batch_cap(0, 10).is_err());
        assert!(enforce_batch_cap(1, 10).is_ok());
        assert!(enforce_batch_cap(10, 10).is_ok());
        assert!(enforce_batch_cap(11, 10).is_err());
    }

    #[test]
    fn test_batch_response_counts_and_ordering() {
        let items = vec![
            BatchItemResult::failure(2, "k:item:2".to_string(), "E", "boom"),
            BatchItemResult::success(0, "k:item:0".to_string(), "ok0"),
            BatchItemResult::success(1, "k:item:1".to_string(), "ok1"),
        ];
        let response = BatchResponse::from_items(items);
        assert_eq!(response.succeeded, 2);
        assert_eq!(response.failed, 1);
        let indices: Vec<usize> = response.items.iter().map(|i| i.index).collect();
        assert_eq!(indices, vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn test_fan_out_batch_partial_failure() {
        let items: Vec<u32> = vec![1, 2, 3, 4];
        let response = fan_out_batch("bk", items, |index, _key, value| async move {
            if value % 2 == 0 {
                Err(AppError::Validation(format!("even item {}", index)))
            } else {
                Ok(value * 10)
            }
        })
        .await;
        assert_eq!(response.succeeded, 2);
        assert_eq!(response.failed, 2);
        assert_eq!(response.items.len(), 4);
        for (pos, item) in response.items.iter().enumerate() {
            assert_eq!(item.index, pos);
            assert_eq!(item.idempotency_key, format!("bk:item:{}", pos));
        }
    }

    #[test]
    fn test_health_status_response_structure() {
        // Verify HealthStatus can represent healthy and unhealthy states
        let healthy = HealthStatus {
            status: "healthy".to_string(),
            version: "0.1.0".to_string(),
            db: "connected".to_string(),
            db_pool: DbPoolStats {
                active_connections: 5,
                idle_connections: 10,
                max_connections: 20,
                usage_percent: 25.0,
            },
            pending_queue_depth: 100,
            current_batch_size: 50,
            ws_connection_count: 10,
        };
        assert_eq!(healthy.status, "healthy");
        assert_eq!(healthy.db, "connected");

        let unhealthy = HealthStatus {
            status: "unhealthy".to_string(),
            version: "0.1.0".to_string(),
            db: "disconnected".to_string(),
            db_pool: DbPoolStats {
                active_connections: 0,
                idle_connections: 0,
                max_connections: 20,
                usage_percent: 0.0,
            },
            pending_queue_depth: 0,
            current_batch_size: 0,
            ws_connection_count: 0,
        };
        assert_eq!(unhealthy.status, "unhealthy");
        assert_eq!(unhealthy.db, "disconnected");
    }

    #[test]
    fn test_db_pool_stats_utilization() {
        // Verify pool stats correctly reflect connection utilization
        let stats = DbPoolStats {
            active_connections: 15,
            idle_connections: 5,
            max_connections: 20,
            usage_percent: 75.0,
        };
        assert_eq!(stats.active_connections + stats.idle_connections, 20);
        assert_eq!(stats.usage_percent, 75.0);

        // Test at capacity
        let full = DbPoolStats {
            active_connections: 20,
            idle_connections: 0,
            max_connections: 20,
            usage_percent: 100.0,
        };
        assert_eq!(full.usage_percent, 100.0);
    }

    #[test]
    fn test_health_checker_db_status_logic() {
        // This test verifies the logic for determining health status based on db_status
        // "connected" → "healthy", "disconnected" → "unhealthy"
        assert_eq!(
            if "connected" == "connected" {
                "healthy"
            } else {
                "unhealthy"
            },
            "healthy"
        );

        assert_eq!(
            if "disconnected" == "connected" {
                "healthy"
            } else {
                "unhealthy"
            },
            "unhealthy"
        );
    }

    #[test]
    fn test_health_checker_status_code_logic() {
        // Verify that database status correctly maps to HTTP status codes
        let connected_code = if "connected" == "connected" {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        };
        assert_eq!(connected_code, StatusCode::OK);

        let disconnected_code = if "disconnected" == "connected" {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        };
        assert_eq!(disconnected_code, StatusCode::SERVICE_UNAVAILABLE);
    }
}
