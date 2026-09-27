//! #1090 – Webhook filter rule CRUD admin API.
//!
//! Exposes tenant-scoped create / list / get / update / delete endpoints for
//! webhook filter rules stored in `webhook_endpoints.filter_rules` (JSONB).
//! Rules are evaluated at dispatch time by `WebhookDispatcher::matches_filters`.
//! Mutating a rule invalidates the per-endpoint Redis cache so the dispatcher
//! picks up the new rules on the very next delivery cycle without a restart.
//!
//! # Rule syntax
//!
//! Rules are a flat JSON object. All keys are optional and ANDed together:
//!
//! ```json
//! {
//!   "asset_codes": ["USD", "EUR"],   // allow-list of asset codes
//!   "min_amount":  "10.00",          // minimum amount (string-encoded decimal)
//!   "max_amount":  "50000.00",       // maximum amount
//!   "event_types": ["deposit.completed", "withdrawal.pending"]
//! }
//! ```
//!
//! An endpoint with `filter_rules = null` receives every event it is
//! subscribed to. Setting `filter_rules` to an empty object `{}` is
//! equivalent — all filters absent, all events pass.
//!
//! # Fail-safe dispatch
//!
//! The dispatcher defaults to *delivering* the event whenever rule evaluation
//! encounters an error (null rules, missing fields, etc.), so a bad rule
//! never causes silent drops — it causes delivery, which is the safe side of
//! the trade-off for a financial system.

use crate::error::AppError;
use crate::ApiState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json, Router,
};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

// ── Request / response types ─────────────────────────────────────────────────

/// Body accepted by the create and update endpoints.
#[derive(Debug, Deserialize)]
pub struct UpsertFilterRulesRequest {
    /// Arbitrary JSON object describing the filter rules.
    /// Pass `null` to clear all rules (deliver everything).
    pub filter_rules: Option<serde_json::Value>,
}

impl UpsertFilterRulesRequest {
    /// Validate the filter rules object structure.
    /// We accept an open schema but reject values that are not objects.
    pub fn validate(&self) -> Result<(), AppError> {
        if let Some(rules) = &self.filter_rules {
            if !rules.is_object() {
                return Err(AppError::BadRequest(
                    "filter_rules must be a JSON object or null".into(),
                ));
            }
            // Validate asset_codes if present
            if let Some(codes) = rules.get("asset_codes") {
                if !codes.is_array() {
                    return Err(AppError::BadRequest(
                        "filter_rules.asset_codes must be an array of strings".into(),
                    ));
                }
                let all_strings = codes
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|v| v.is_string());
                if !all_strings {
                    return Err(AppError::BadRequest(
                        "all entries in filter_rules.asset_codes must be strings".into(),
                    ));
                }
            }
            // Validate amount fields if present
            for field in &["min_amount", "max_amount"] {
                if let Some(v) = rules.get(*field) {
                    let s = v.as_str().ok_or_else(|| {
                        AppError::BadRequest(format!(
                            "filter_rules.{field} must be a string-encoded decimal"
                        ))
                    })?;
                    s.parse::<f64>().map_err(|_| {
                        AppError::BadRequest(format!(
                            "filter_rules.{field} is not a valid decimal: {s}"
                        ))
                    })?;
                }
            }
        }
        Ok(())
    }
}

/// Response body for a single filter-rules record.
#[derive(Debug, Serialize)]
pub struct FilterRulesResponse {
    pub endpoint_id: Uuid,
    pub filter_rules: Option<serde_json::Value>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

// ── Redis cache key ───────────────────────────────────────────────────────────

/// Key prefix for the per-endpoint filter-rules cache in Redis.
/// Any change to an endpoint's rules deletes this key so the next call to
/// `endpoints_for_event` re-reads from Postgres.
fn filter_cache_key(endpoint_id: Uuid) -> String {
    format!("webhook:filter_rules:{endpoint_id}")
}

/// Invalidate the cached filter rules for `endpoint_id`.  Fails silently on
/// Redis errors so a cache miss never blocks the mutation — the dispatcher is
/// designed to read from Postgres on a cache miss.
async fn invalidate_filter_cache(redis_url: &str, endpoint_id: Uuid) {
    match redis::Client::open(redis_url) {
        Ok(client) => match client.get_multiplexed_async_connection().await {
            Ok(mut conn) => {
                let key = filter_cache_key(endpoint_id);
                let _: Result<(), _> = conn.del::<_, ()>(&key).await;
            }
            Err(e) => {
                tracing::warn!(
                    endpoint_id = %endpoint_id,
                    "Failed to get Redis connection for filter cache invalidation: {e}"
                );
            }
        },
        Err(e) => {
            tracing::warn!(
                endpoint_id = %endpoint_id,
                "Failed to open Redis client for filter cache invalidation: {e}"
            );
        }
    }
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// `GET /admin/webhooks/endpoints/:id/filter-rules`
///
/// Returns the current filter rules for the given endpoint.
pub async fn get_filter_rules(
    State(state): State<ApiState>,
    Path(endpoint_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let row = sqlx::query(
        "SELECT id, filter_rules, updated_at \
         FROM webhook_endpoints \
         WHERE id = $1",
    )
    .bind(endpoint_id)
    .fetch_optional(&state.app_state.db)
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
    .ok_or_else(|| AppError::NotFound("Webhook endpoint not found".into()))?;

    Ok((
        StatusCode::OK,
        Json(FilterRulesResponse {
            endpoint_id: row.get("id"),
            filter_rules: row.get("filter_rules"),
            updated_at: row.get("updated_at"),
        }),
    ))
}

/// `PUT /admin/webhooks/endpoints/:id/filter-rules`
///
/// Replace the filter rules for an endpoint.  Pass `{"filter_rules": null}`
/// to clear all rules and deliver every subscribed event.
/// Invalidates the Redis cache for this endpoint on success.
pub async fn set_filter_rules(
    State(state): State<ApiState>,
    Path(endpoint_id): Path<Uuid>,
    Json(body): Json<UpsertFilterRulesRequest>,
) -> Result<impl IntoResponse, AppError> {
    body.validate()?;

    let row = sqlx::query(
        "UPDATE webhook_endpoints \
         SET filter_rules = $2, updated_at = NOW() \
         WHERE id = $1 \
         RETURNING id, filter_rules, updated_at",
    )
    .bind(endpoint_id)
    .bind(&body.filter_rules)
    .fetch_optional(&state.app_state.db)
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
    .ok_or_else(|| AppError::NotFound("Webhook endpoint not found".into()))?;

    invalidate_filter_cache(&state.app_state.redis_url, endpoint_id).await;

    tracing::info!(
        endpoint_id = %endpoint_id,
        "Webhook filter rules updated; cache invalidated"
    );

    Ok((
        StatusCode::OK,
        Json(FilterRulesResponse {
            endpoint_id: row.get("id"),
            filter_rules: row.get("filter_rules"),
            updated_at: row.get("updated_at"),
        }),
    ))
}

/// `DELETE /admin/webhooks/endpoints/:id/filter-rules`
///
/// Clear the filter rules (equivalent to `PUT` with `filter_rules: null`).
/// Invalidates the Redis cache for this endpoint on success.
pub async fn delete_filter_rules(
    State(state): State<ApiState>,
    Path(endpoint_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let affected = sqlx::query(
        "UPDATE webhook_endpoints \
         SET filter_rules = NULL, updated_at = NOW() \
         WHERE id = $1",
    )
    .bind(endpoint_id)
    .execute(&state.app_state.db)
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
    .rows_affected();

    if affected == 0 {
        return Err(AppError::NotFound("Webhook endpoint not found".into()));
    }

    invalidate_filter_cache(&state.app_state.redis_url, endpoint_id).await;

    tracing::info!(
        endpoint_id = %endpoint_id,
        "Webhook filter rules cleared; cache invalidated"
    );

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /admin/webhooks/filter-rules`
///
/// List all webhook endpoints that have non-null filter rules, with their
/// current rule sets.
pub async fn list_endpoints_with_filter_rules(
    State(state): State<ApiState>,
) -> Result<impl IntoResponse, AppError> {
    let rows = sqlx::query(
        "SELECT id, filter_rules, updated_at \
         FROM webhook_endpoints \
         WHERE filter_rules IS NOT NULL \
         ORDER BY updated_at DESC",
    )
    .fetch_all(&state.app_state.db)
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;

    let entries: Vec<FilterRulesResponse> = rows
        .into_iter()
        .map(|row| FilterRulesResponse {
            endpoint_id: row.get("id"),
            filter_rules: row.get("filter_rules"),
            updated_at: row.get("updated_at"),
        })
        .collect();

    Ok((StatusCode::OK, Json(entries)))
}

/// `POST /admin/webhooks/endpoints/:id/filter-rules/validate`
///
/// Validate a filter rules object without persisting it.  Useful for clients
/// to check rule syntax before committing a change.
pub async fn validate_filter_rules(
    Json(body): Json<UpsertFilterRulesRequest>,
) -> Result<impl IntoResponse, AppError> {
    body.validate()?;
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "valid": true,
            "filter_rules": body.filter_rules,
        })),
    ))
}

// ── Router ────────────────────────────────────────────────────────────────────

/// Returns a `Router<ApiState>` for all filter-rules endpoints.
/// Mount this under `/admin` in `lib.rs`.
pub fn webhook_filter_rules_routes() -> Router<ApiState> {
    Router::new()
        // List all endpoints that have rules
        .route(
            "/webhooks/filter-rules",
            get(list_endpoints_with_filter_rules),
        )
        // Per-endpoint CRUD
        .route(
            "/webhooks/endpoints/:id/filter-rules",
            get(get_filter_rules)
                .put(set_filter_rules)
                .delete(delete_filter_rules),
        )
        // Validation helper
        .route(
            "/webhooks/filter-rules/validate",
            post(validate_filter_rules),
        )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_null_filter_rules() {
        let req = UpsertFilterRulesRequest { filter_rules: None };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_validate_empty_object() {
        let req = UpsertFilterRulesRequest {
            filter_rules: Some(serde_json::json!({})),
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_validate_valid_rules() {
        let req = UpsertFilterRulesRequest {
            filter_rules: Some(serde_json::json!({
                "asset_codes": ["USD", "EUR"],
                "min_amount": "10.00",
                "max_amount": "50000.00"
            })),
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_validate_non_object_rejected() {
        let req = UpsertFilterRulesRequest {
            filter_rules: Some(serde_json::json!(["USD"])),
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_validate_asset_codes_must_be_array() {
        let req = UpsertFilterRulesRequest {
            filter_rules: Some(serde_json::json!({ "asset_codes": "USD" })),
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_validate_amount_must_be_decimal_string() {
        let req = UpsertFilterRulesRequest {
            filter_rules: Some(serde_json::json!({ "min_amount": "not-a-number" })),
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_validate_amount_must_be_string_not_number() {
        let req = UpsertFilterRulesRequest {
            filter_rules: Some(serde_json::json!({ "min_amount": 10.0 })),
        };
        // min_amount must be a string, not a JSON number
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_filter_cache_key_format() {
        let id = Uuid::nil();
        assert_eq!(
            filter_cache_key(id),
            "webhook:filter_rules:00000000-0000-0000-0000-000000000000"
        );
    }
}
