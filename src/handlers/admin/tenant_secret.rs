use crate::error::AppError;
use crate::ApiState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct RotateTenantSecretRequest {
    /// Optional explicit new API key. If omitted, a cryptographically secure random key is generated.
    pub new_api_key: Option<String>,
    /// Grace period in seconds during which both old and new credentials validate (default: 3600).
    pub grace_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct RotateTenantSecretQuery {
    pub grace_seconds: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RotateTenantSecretResponse {
    pub tenant_id: Uuid,
    pub api_key: String,
    pub grace_seconds: u64,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RevokeTenantSecretResponse {
    pub tenant_id: Uuid,
    pub revoked: bool,
    pub message: String,
}

/// POST /admin/tenants/:tenant_id/rotate-secret — Rotate a tenant's API key with a configurable grace period.
pub async fn rotate_tenant_secret(
    State(state): State<ApiState>,
    Path(tenant_id): Path<Uuid>,
    Query(query): Query<RotateTenantSecretQuery>,
    payload: Option<Json<RotateTenantSecretRequest>>,
) -> Result<impl IntoResponse, AppError> {
    let payload = payload.map(|Json(p)| p).unwrap_or_default();
    let grace_seconds = payload
        .grace_seconds
        .or(query.grace_seconds)
        .unwrap_or(crate::db::queries::DEFAULT_ROTATION_GRACE_SECONDS);

    let result = crate::db::queries::rotate_tenant_api_key(
        &state.app_state.db,
        tenant_id,
        payload.new_api_key,
        grace_seconds,
        "admin_api",
    )
    .await
    .map_err(|e| match e {
        sqlx::Error::RowNotFound => AppError::NotFound("tenant not found or inactive".to_string()),
        sqlx::Error::Decode(msg) => AppError::BadRequest(msg.to_string()),
        other => AppError::DatabaseError(other.to_string()),
    })?;

    // Refresh in-memory tenant configs if needed
    if let Err(e) = state.app_state.load_tenant_configs().await {
        tracing::warn!(
            tenant_id = %tenant_id,
            error = %e,
            "tenant secret rotated in database, but in-memory reload failed"
        );
    }

    // Spawn background task to automatically clean up expired secret at grace period end
    if grace_seconds > 0 {
        let pool = state.app_state.db.clone();
        let app_state = state.app_state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(grace_seconds)).await;
            if let Err(e) =
                crate::db::queries::revoke_tenant_previous_secret(&pool, tenant_id, "system_grace_period_expiry")
                    .await
            {
                tracing::error!(
                    tenant_id = %tenant_id,
                    error = %e,
                    "failed to auto-revoke tenant previous secret at grace period expiry"
                );
            } else {
                let _ = app_state.load_tenant_configs().await;
            }
        });
    }

    let message = if grace_seconds > 0 {
        format!(
            "Tenant secret rotated successfully. Both old and new secrets are valid for {} seconds.",
            grace_seconds
        )
    } else {
        "Tenant secret rotated successfully with immediate revocation of previous secret.".to_string()
    };

    Ok((
        StatusCode::OK,
        Json(RotateTenantSecretResponse {
            tenant_id: result.tenant_id,
            api_key: result.new_api_key,
            grace_seconds,
            expires_at: result.grace_period_expires_at,
            message,
        }),
    ))
}

/// POST /admin/tenants/:tenant_id/revoke-secret — Revoke previous API key immediately, terminating grace period.
pub async fn revoke_tenant_secret(
    State(state): State<ApiState>,
    Path(tenant_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let revoked = crate::db::queries::revoke_tenant_previous_secret(
        &state.app_state.db,
        tenant_id,
        "admin_api",
    )
    .await
    .map_err(|e| match e {
        sqlx::Error::RowNotFound => AppError::NotFound("tenant not found or inactive".to_string()),
        other => AppError::DatabaseError(other.to_string()),
    })?;

    let message = if revoked {
        "Previous tenant secret revoked and grace period terminated.".to_string()
    } else {
        "No active grace period or previous secret found for tenant.".to_string()
    };

    Ok((
        StatusCode::OK,
        Json(RevokeTenantSecretResponse {
            tenant_id,
            revoked,
            message,
        }),
    ))
}
