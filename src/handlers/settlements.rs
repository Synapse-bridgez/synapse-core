use crate::error::AppError;
use crate::utils::cursor as cursor_util;
use crate::validation::{validate_max_len, validate_required};
use crate::ApiState;
use axum::{
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct SettlementListQuery {
    pub cursor: Option<String>,
    pub limit: Option<i64>,
    /// "forward" (default) or "backward"
    pub direction: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SettlementListResponse {
    pub settlements: Vec<crate::db::models::Settlement>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[utoipa::path(
    get,
    path = "/settlements",
    params(
        ("cursor" = Option<String>, Query, description = "Pagination cursor"),
        ("limit" = Option<i64>, Query, description = "Page size (1-100, default 10)"),
        ("direction" = Option<String>, Query, description = "\"forward\" (default) or \"backward\""),
    ),
    responses(
        (status = 200, description = "List of settlements", body = SettlementListResponse),
        (status = 400, description = "Invalid cursor"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Settlements"
)]
pub async fn list_settlements(
    State(state): State<ApiState>,
    Query(params): Query<SettlementListQuery>,
) -> Result<impl IntoResponse, AppError> {
    let limit = params.limit.unwrap_or(10).clamp(1, 100);
    let backward = params.direction.as_deref() == Some("backward");

    let decoded_cursor = if let Some(ref c) = params.cursor {
        match cursor_util::decode(c) {
            Ok(pair) => Some(pair),
            Err(e) => return Err(AppError::BadRequest(format!("invalid cursor: {}", e))),
        }
    } else {
        None
    };

    let fetch_limit = limit + 1;
    let (pool, replica_used) = state.app_state.pool_manager.read_pool().await;
    let mut settlements =
        crate::db::queries::list_settlements_cursor(pool, fetch_limit, decoded_cursor, backward)
            .await?;

    let has_more = settlements.len() as i64 > limit;
    if has_more {
        settlements.truncate(limit as usize);
    }

    let next_cursor = settlements
        .last()
        .map(|s| cursor_util::encode(s.created_at, s.id));

    let body = SettlementListResponse {
        settlements,
        next_cursor,
        has_more,
    };

    let mut response: Response = Json(body).into_response();
    if replica_used {
        response
            .headers_mut()
            .insert("X-Read-Consistency", HeaderValue::from_static("eventual"));
    }

    Ok(response)
}

#[utoipa::path(
    get,
    path = "/settlements/{id}",
    params(
        ("id" = Uuid, Path, description = "Settlement ID"),
    ),
    responses(
        (status = 200, description = "Settlement details", body = crate::db::models::Settlement),
        (status = 404, description = "Settlement not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Settlements"
)]
pub async fn get_settlement(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let (pool, replica_used) = state.app_state.pool_manager.read_pool().await;
    let settlement = crate::db::queries::get_settlement(pool, id)
        .await
        .map_err(|e| {
            if matches!(e, sqlx::Error::RowNotFound) {
                AppError::NotFound(format!("Settlement {} not found", id))
            } else {
                AppError::from(e)
            }
        })?;

    let mut response: Response = Json(settlement).into_response();
    if replica_used {
        response
            .headers_mut()
            .insert("X-Read-Consistency", HeaderValue::from_static("eventual"));
    }

    Ok(response)
}

/// Request body for admin settlement status changes.
#[derive(Debug, Deserialize)]
pub struct UpdateSettlementStatusRequest {
    pub status: String,
    pub reason: Option<String>,
    /// New total amount — only meaningful when transitioning to "adjusted".
    pub new_total: Option<String>,
    /// Actor performing the change (defaults to "admin").
    pub actor: Option<String>,
}

impl UpdateSettlementStatusRequest {
    /// Validates the settlement status update request.
    ///
    /// Returns a 400 Bad Request error if:
    /// - status is empty or missing
    /// - status exceeds 50 characters
    /// - reason exceeds 255 characters
    ///
    /// Note: The 'actor' field is accepted in the request for backwards compatibility
    /// but is NOT validated and is NOT used. The actor is always set server-side to "admin"
    /// to prevent privilege escalation through audit trail impersonation (issue #952).
    pub fn validate(&self) -> Result<(), AppError> {
        validate_required("status", &self.status)
            .map_err(|e| AppError::BadRequest(e.to_string()))?;

        validate_max_len("status", &self.status, 50)
            .map_err(|e| AppError::BadRequest(e.to_string()))?;

        if let Some(reason) = &self.reason {
            validate_max_len("reason", reason, 255)
                .map_err(|e| AppError::BadRequest(e.to_string()))?;
        }

        Ok(())
    }
}

/// PATCH /admin/settlements/:id/status
/// Allowed transitions: completed→pending_review, →disputed, pending_review→adjusted/voided/disputed,
/// disputed→adjusted/voided/pending_review.
///
/// # Security Note
/// The actor field in the request body is accepted for backwards compatibility but is
/// ignored. The actor value is always derived server-side (currently set to "admin") to
/// prevent clients from spoofing audit trail entries with arbitrary actor values
/// (e.g., impersonating colleagues or hiding who made the change).
pub async fn update_settlement_status(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateSettlementStatusRequest>,
) -> Result<impl IntoResponse, AppError> {
    payload.validate()?;

    let new_total: Option<sqlx::types::BigDecimal> = match payload.new_total.as_deref() {
        Some(s) => match s.parse() {
            Ok(v) => Some(v),
            Err(_) => return Err(AppError::BadRequest("invalid new_total".to_string())),
        },
        None => None,
    };

    // Always use "admin" as actor, regardless of what client supplies.
    // This prevents audit trail spoofing. In future, derive from authenticated principal.
    let actor = "admin";
    let service = crate::services::SettlementService::new(state.app_state.db.clone());

    let settlement = service
        .update_status(
            id,
            &payload.status,
            payload.reason.as_deref(),
            new_total.as_ref(),
            actor,
        )
        .await?;

    Ok((StatusCode::OK, Json(settlement)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_update_settlement_status_valid() {
        let req = UpdateSettlementStatusRequest {
            status: "pending".to_string(),
            reason: Some("testing".to_string()),
            new_total: None,
            actor: Some("admin".to_string()),
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_update_settlement_status_empty_status() {
        let req = UpdateSettlementStatusRequest {
            status: "".to_string(),
            reason: None,
            new_total: None,
            actor: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_update_settlement_status_status_too_long() {
        let req = UpdateSettlementStatusRequest {
            status: "a".repeat(51),
            reason: None,
            new_total: None,
            actor: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_update_settlement_status_reason_too_long() {
        let req = UpdateSettlementStatusRequest {
            status: "pending".to_string(),
            reason: Some("a".repeat(256)),
            new_total: None,
            actor: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_update_settlement_status_actor_field_not_validated() {
        // The actor field is accepted for backwards compatibility but should not be validated
        // This prevents the field from being rejected if an extremely long value is passed
        let req = UpdateSettlementStatusRequest {
            status: "pending".to_string(),
            reason: None,
            new_total: None,
            actor: Some("a".repeat(1000)), // Very long actor value
        };
        // Validation should pass - actor is NOT validated
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_update_settlement_status_actor_field_ignored() {
        // The actor field in the request is ignored; validation passes regardless of its value
        let req_with_actor = UpdateSettlementStatusRequest {
            status: "pending".to_string(),
            reason: Some("manual review".to_string()),
            new_total: None,
            actor: Some("malicious_actor".to_string()),
        };
        let req_without_actor = UpdateSettlementStatusRequest {
            status: "pending".to_string(),
            reason: Some("manual review".to_string()),
            new_total: None,
            actor: None,
        };
        // Both should validate identically - actor field should not affect validation
        assert_eq!(
            req_with_actor.validate().is_ok(),
            req_without_actor.validate().is_ok()
        );
    }

    #[test]
    fn test_update_settlement_status_actor_too_long() {
        let req = UpdateSettlementStatusRequest {
            status: "pending".to_string(),
            reason: None,
            new_total: None,
            actor: Some("a".repeat(51)),
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_update_settlement_status_boundary_values() {
        // Test status at max length
        let req = UpdateSettlementStatusRequest {
            status: "a".repeat(50),
            reason: None,
            new_total: None,
            actor: None,
        };
        assert!(req.validate().is_ok());

        // Test reason at max length
        let req = UpdateSettlementStatusRequest {
            status: "pending".to_string(),
            reason: Some("a".repeat(255)),
            new_total: None,
            actor: None,
        };
        assert!(req.validate().is_ok());

        // Test actor at max length
        let req = UpdateSettlementStatusRequest {
            status: "pending".to_string(),
            reason: None,
            new_total: None,
            actor: Some("a".repeat(50)),
        };
        assert!(req.validate().is_ok());
    }
}
