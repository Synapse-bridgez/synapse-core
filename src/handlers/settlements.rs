use crate::error::AppError;
use crate::swap::settlement::submission::{
    SubmissionAttempt, SubmissionOutcome, SubmissionStore,
};
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
    tenant: crate::tenant::TenantContext,
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
    let mut settlements = crate::db::queries::list_settlements_cursor_for_tenant(
        &pool,
        fetch_limit,
        decoded_cursor,
        backward,
        tenant.tenant_id,
    )
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
    tenant: crate::tenant::TenantContext,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let (pool, replica_used) = state.app_state.pool_manager.read_pool().await;
    let settlement = crate::db::queries::get_settlement_for_tenant(&pool, id, tenant.tenant_id)
        .await
        .map_err(|e| {
            if matches!(e, sqlx::Error::RowNotFound) {
                // See webhook::get_transaction for why this stays indistinguishable
                // from "no such settlement" in the response but is still counted.
                tracing::info!(
                    counter.cross_tenant_denial_total = 1u64,
                    tenant_id = %tenant.tenant_id,
                    settlement_id = %id,
                    "get_settlement: not found or not visible to this tenant"
                );
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

/// Query parameters for listing persisted submission attempts of a settlement.
#[derive(Debug, Deserialize)]
pub struct SubmissionAttemptsQuery {
    pub cursor: Option<String>,
    pub limit: Option<i64>,
    /// "forward" (default) or "backward"
    pub direction: Option<String>,
}

/// A single persisted submission attempt, exposed for reconciliation.
#[derive(Debug, Serialize, ToSchema)]
pub struct SubmissionAttemptView {
    pub id: Uuid,
    pub settlement_id: Uuid,
    pub source_account: String,
    pub channel_account: Option<String>,
    pub sequence_number: i64,
    pub transaction_hash: Option<String>,
    pub status: String,
    pub outcome: Option<String>,
    pub error_code: Option<String>,
    pub attempt_number: i32,
    pub ledger: Option<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<SubmissionAttempt> for SubmissionAttemptView {
    fn from(attempt: SubmissionAttempt) -> Self {
        let outcome = attempt.outcome.map(|o| match o {
            SubmissionOutcome::Pending => "pending".to_string(),
            SubmissionOutcome::Confirmed => "confirmed".to_string(),
            SubmissionOutcome::Duplicate => "duplicate".to_string(),
            SubmissionOutcome::Retryable => "retryable".to_string(),
            SubmissionOutcome::Terminal => "terminal".to_string(),
        });

        Self {
            id: attempt.id,
            settlement_id: attempt.settlement_id,
            source_account: attempt.source_account,
            channel_account: attempt.channel_account,
            sequence_number: attempt.sequence_number,
            transaction_hash: attempt.transaction_hash,
            status: attempt.status.as_str().to_string(),
            outcome,
            error_code: attempt.error_code,
            attempt_number: attempt.attempt_number,
            ledger: attempt.ledger,
            created_at: attempt.created_at,
            updated_at: attempt.updated_at,
        }
    }
}

/// Paginated list of submission attempts for a settlement.
#[derive(Debug, Serialize, ToSchema)]
pub struct SubmissionAttemptsResponse {
    pub settlement_id: Uuid,
    pub attempts: Vec<SubmissionAttemptView>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
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
    /// - actor exceeds 50 characters
    pub fn validate(&self) -> Result<(), AppError> {
        validate_required("status", &self.status)
            .map_err(|e| AppError::BadRequest(e.to_string()))?;

        validate_max_len("status", &self.status, 50)
            .map_err(|e| AppError::BadRequest(e.to_string()))?;

        if let Some(reason) = &self.reason {
            validate_max_len("reason", reason, 255)
                .map_err(|e| AppError::BadRequest(e.to_string()))?;
        }

        if let Some(actor) = &self.actor {
            validate_max_len("actor", actor, 50)
                .map_err(|e| AppError::BadRequest(e.to_string()))?;
        }

        Ok(())
    }
}

#[utoipa::path(
    get,
    path = "/settlements/{id}/submissions",
    params(
        ("id" = Uuid, Path, description = "Settlement ID"),
        ("cursor" = Option<String>, Query, description = "Pagination cursor"),
        ("limit" = Option<i64>, Query, description = "Page size (1-100, default 20)"),
        ("direction" = Option<String>, Query, description = "\"forward\" (default) or \"backward\""),
    ),
    responses(
        (status = 200, description = "Submission attempts for the settlement", body = SubmissionAttemptsResponse),
        (status = 400, description = "Invalid cursor"),
        (status = 404, description = "Settlement not found"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "Settlements"
)]
pub async fn list_settlement_submissions(
    State(state): State<ApiState>,
    tenant: crate::tenant::TenantContext,
    Path(id): Path<Uuid>,
    Query(params): Query<SubmissionAttemptsQuery>,
) -> Result<impl IntoResponse, AppError> {
    let limit = params.limit.unwrap_or(20).clamp(1, 100);
    let backward = params.direction.as_deref() == Some("backward");

    let decoded_cursor = if let Some(ref c) = params.cursor {
        match cursor_util::decode(c) {
            Ok(pair) => Some(pair),
            Err(e) => return Err(AppError::BadRequest(format!("invalid cursor: {}", e))),
        }
    } else {
        None
    };

    let (pool, replica_used) = state.app_state.pool_manager.read_pool().await;

    // Ensure the settlement exists and is visible to this tenant before exposing
    // its submission attempts.
    crate::db::queries::get_settlement_for_tenant(&pool, id, tenant.tenant_id)
        .await
        .map_err(|e| {
            if matches!(e, sqlx::Error::RowNotFound) {
                tracing::info!(
                    counter.cross_tenant_denial_total = 1u64,
                    tenant_id = %tenant.tenant_id,
                    settlement_id = %id,
                    "list_settlement_submissions: not found or not visible to this tenant"
                );
                AppError::NotFound(format!("Settlement {} not found", id))
            } else {
                AppError::from(e)
            }
        })?;

    let store = SubmissionStore::new(pool.clone());
    let fetch_limit = limit + 1;
    let mut attempts = store
        .list_attempts_for_settlement(id, fetch_limit, decoded_cursor, backward)
        .await?;

    let has_more = attempts.len() as i64 > limit;
    if has_more {
        attempts.truncate(limit as usize);
    }

    let next_cursor = attempts
        .last()
        .map(|a| cursor_util::encode(a.created_at, a.id));

    let body = SubmissionAttemptsResponse {
        settlement_id: id,
        attempts: attempts.into_iter().map(SubmissionAttemptView::from).collect(),
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

/// PATCH /admin/settlements/:id/status
/// Allowed transitions: completed→pending_review, →disputed, pending_review→adjusted/voided/disputed,
/// disputed→adjusted/voided/pending_review.
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

    let actor = payload.actor.as_deref().unwrap_or("admin");
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
    fn test_submission_attempt_view_maps_outcome() {
        let now = chrono::Utc::now();
        let attempt = SubmissionAttempt {
            id: Uuid::new_v4(),
            settlement_id: Uuid::new_v4(),
            source_account: "GSOURCE".to_string(),
            channel_account: Some("GCHANNEL".to_string()),
            sequence_number: 42,
            transaction_hash: Some("abc123".to_string()),
            status: crate::swap::settlement::submission::AttemptStatus::Submitted,
            outcome: Some(SubmissionOutcome::Duplicate),
            error_code: Some("tx_bad_seq".to_string()),
            attempt_number: 2,
            ledger: Some(100),
            created_at: now,
            updated_at: now,
        };

        let view = SubmissionAttemptView::from(attempt);
        assert_eq!(view.outcome.as_deref(), Some("duplicate"));
        assert_eq!(view.status, "submitted");
        assert_eq!(view.attempt_number, 2);
        assert_eq!(view.sequence_number, 42);
    }

    #[test]
    fn test_submission_attempt_view_without_outcome() {
        let now = chrono::Utc::now();
        let attempt = SubmissionAttempt {
            id: Uuid::new_v4(),
            settlement_id: Uuid::new_v4(),
            source_account: "GSOURCE".to_string(),
            channel_account: None,
            sequence_number: 1,
            transaction_hash: None,
            status: crate::swap::settlement::submission::AttemptStatus::Allocated,
            outcome: None,
            error_code: None,
            attempt_number: 1,
            ledger: None,
            created_at: now,
            updated_at: now,
        };

        let view = SubmissionAttemptView::from(attempt);
        assert!(view.outcome.is_none());
        assert_eq!(view.status, "allocated");
    }

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
