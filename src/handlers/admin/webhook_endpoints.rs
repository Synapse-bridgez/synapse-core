//! Bulk webhook endpoint management API
//!
//! Provides batch create/update/disable operations for webhook endpoints.
//! All batch operations are atomic: either all operations succeed or none are committed.

use crate::error::AppError;
use crate::ApiState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

const MAX_BATCH_SIZE: usize = 100;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CreateWebhookEndpointRequest {
    pub url: String,
    pub secret: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWebhookEndpointRequest {
    pub id: Uuid,
    pub url: Option<String>,
    pub secret: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DisableWebhookEndpointRequest {
    pub id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct BatchWebhookOperation {
    pub operation: String, // "create", "update", or "disable"
    pub create: Option<CreateWebhookEndpointRequest>,
    pub update: Option<UpdateWebhookEndpointRequest>,
    pub disable: Option<DisableWebhookEndpointRequest>,
}

#[derive(Debug, Serialize)]
pub struct WebhookEndpoint {
    pub id: String,
    pub url: String,
    pub secret: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct BatchOperationResult {
    pub success: bool,
    pub operation_index: usize,
    pub operation: String,
    pub endpoint_id: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BatchWebhookResponse {
    pub total_operations: usize,
    pub successful: usize,
    pub failed: usize,
    pub results: Vec<BatchOperationResult>,
}

impl BatchWebhookOperation {
    /// Validate the batch operation structure
    pub fn validate(&self) -> Result<(), AppError> {
        match self.operation.as_str() {
            "create" => {
                let req = self
                    .create
                    .as_ref()
                    .ok_or_else(|| AppError::BadRequest("create field required for create operation".to_string()))?;

                if req.url.is_empty() {
                    return Err(AppError::Validation("URL cannot be empty".to_string()));
                }
                if req.url.len() > 500 {
                    return Err(AppError::Validation("URL must not exceed 500 characters".to_string()));
                }
                if let Some(secret) = &req.secret {
                    if secret.len() > 255 {
                        return Err(AppError::Validation("Secret must not exceed 255 characters".to_string()));
                    }
                }
                Ok(())
            }
            "update" => {
                let _req = self
                    .update
                    .as_ref()
                    .ok_or_else(|| AppError::BadRequest("update field required for update operation".to_string()))?;
                Ok(())
            }
            "disable" => {
                let _req = self
                    .disable
                    .as_ref()
                    .ok_or_else(|| AppError::BadRequest("disable field required for disable operation".to_string()))?;
                Ok(())
            }
            _ => Err(AppError::BadRequest(format!(
                "Invalid operation: {}. Must be 'create', 'update', or 'disable'",
                self.operation
            ))),
        }
    }
}

/// Batch create/update/disable webhook endpoints
///
/// Accepts up to 100 operations in a single request. All operations are atomic:
/// either all succeed or none are committed.
///
/// # Errors
/// - `400 Bad Request` – invalid payload or validation error
/// - `500 Internal Server Error` – database error
pub async fn batch_webhook_operations(
    State(state): State<ApiState>,
    Json(operations): Json<Vec<BatchWebhookOperation>>,
) -> impl IntoResponse {
    crate::metrics::admin_webhook_requests_total()
        .add(1, &[opentelemetry::KeyValue::new("operation", "batch")]);

    if operations.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Operations list cannot be empty"
            })),
        )
            .into_response();
    }

    if operations.len() > MAX_BATCH_SIZE {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("Cannot exceed {} operations per batch", MAX_BATCH_SIZE)
            })),
        )
            .into_response();
    }

    // Validate all operations first (fail-fast)
    for op in &operations {
        if let Err(e) = op.validate() {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": e.to_string()
                })),
            )
                .into_response();
        }
    }

    // Check for duplicate URLs within the batch
    let mut seen_urls = std::collections::HashSet::new();
    for op in &operations {
        if op.operation == "create" {
            if let Some(create_req) = &op.create {
                if !seen_urls.insert(create_req.url.clone()) {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({
                            "error": format!("Duplicate URL in batch: {}", create_req.url)
                        })),
                    )
                        .into_response();
                }
            }
        }
    }

    // Execute all operations in a single transaction
    match execute_batch_operations(&state.app_state.db, operations).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": e.to_string()
            })),
        )
            .into_response(),
    }
}

async fn execute_batch_operations(
    pool: &sqlx::PgPool,
    operations: Vec<BatchWebhookOperation>,
) -> Result<BatchWebhookResponse, AppError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    let mut results = Vec::new();
    let mut successful = 0;
    let mut failed = 0;

    for (index, op) in operations.iter().enumerate() {
        let result = match op.operation.as_str() {
            "create" => {
                execute_create_endpoint(&mut tx, op.create.as_ref().unwrap(), index).await
            }
            "update" => {
                execute_update_endpoint(&mut tx, op.update.as_ref().unwrap(), index).await
            }
            "disable" => {
                execute_disable_endpoint(&mut tx, op.disable.as_ref().unwrap(), index).await
            }
            _ => Err(AppError::BadRequest("Invalid operation".to_string())),
        };

        match result {
            Ok((endpoint_id, op_result)) => {
                successful += 1;
                results.push(BatchOperationResult {
                    success: true,
                    operation_index: index,
                    operation: op.operation.clone(),
                    endpoint_id: Some(endpoint_id),
                    error: None,
                });
            }
            Err(e) => {
                failed += 1;
                results.push(BatchOperationResult {
                    success: false,
                    operation_index: index,
                    operation: op.operation.clone(),
                    endpoint_id: None,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    // If any operation failed, rollback the entire transaction
    if failed > 0 {
        tx.rollback()
            .await
            .map_err(|e| AppError::DatabaseError(e.to_string()))?;
    } else {
        tx.commit()
            .await
            .map_err(|e| AppError::DatabaseError(e.to_string()))?;
    }

    Ok(BatchWebhookResponse {
        total_operations: operations.len(),
        successful,
        failed,
        results,
    })
}

async fn execute_create_endpoint(
    tx: &mut Transaction<'_, Postgres>,
    req: &CreateWebhookEndpointRequest,
    _index: usize,
) -> Result<(String, String), AppError> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO webhook_endpoints (id, url, secret, created_at, updated_at)
         VALUES ($1, $2, $3, NOW(), NOW())",
    )
    .bind(id)
    .bind(&req.url)
    .bind(&req.secret)
    .execute(&mut **tx)
    .await
    .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    Ok((id.to_string(), "created".to_string()))
}

async fn execute_update_endpoint(
    tx: &mut Transaction<'_, Postgres>,
    req: &UpdateWebhookEndpointRequest,
    _index: usize,
) -> Result<(String, String), AppError> {
    // Check if endpoint exists
    let existing: Option<(String,)> = sqlx::query_as(
        "SELECT url FROM webhook_endpoints WHERE id = $1",
    )
    .bind(req.id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    if existing.is_none() {
        return Err(AppError::NotFound(format!(
            "Webhook endpoint {} not found",
            req.id
        )));
    }

    // Update only the fields that are provided
    if req.url.is_some() || req.secret.is_some() {
        if let Some(url) = &req.url {
            if url.len() > 500 {
                return Err(AppError::Validation("URL must not exceed 500 characters".to_string()));
            }
        }

        sqlx::query(
            "UPDATE webhook_endpoints
             SET url = COALESCE($2, url),
                 secret = COALESCE($3, secret),
                 updated_at = NOW()
             WHERE id = $1",
        )
        .bind(req.id)
        .bind(&req.url)
        .bind(&req.secret)
        .execute(&mut **tx)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;
    }

    Ok((req.id.to_string(), "updated".to_string()))
}

async fn execute_disable_endpoint(
    tx: &mut Transaction<'_, Postgres>,
    req: &DisableWebhookEndpointRequest,
    _index: usize,
) -> Result<(String, String), AppError> {
    // Check if endpoint exists
    let existing: Option<(String,)> = sqlx::query_as(
        "SELECT id::text FROM webhook_endpoints WHERE id = $1",
    )
    .bind(req.id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    if existing.is_none() {
        return Err(AppError::NotFound(format!(
            "Webhook endpoint {} not found",
            req.id
        )));
    }

    // Delete the endpoint (soft or hard delete based on requirements)
    sqlx::query("DELETE FROM webhook_endpoints WHERE id = $1")
        .bind(req.id)
        .execute(&mut **tx)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    Ok((req.id.to_string(), "disabled".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_create_operation() {
        let op = BatchWebhookOperation {
            operation: "create".to_string(),
            create: Some(CreateWebhookEndpointRequest {
                url: "https://example.com/webhook".to_string(),
                secret: Some("secret123".to_string()),
            }),
            update: None,
            disable: None,
        };
        assert!(op.validate().is_ok());
    }

    #[test]
    fn test_validate_create_empty_url() {
        let op = BatchWebhookOperation {
            operation: "create".to_string(),
            create: Some(CreateWebhookEndpointRequest {
                url: "".to_string(),
                secret: None,
            }),
            update: None,
            disable: None,
        };
        assert!(op.validate().is_err());
    }

    #[test]
    fn test_validate_create_url_too_long() {
        let op = BatchWebhookOperation {
            operation: "create".to_string(),
            create: Some(CreateWebhookEndpointRequest {
                url: "https://example.com/".to_string() + &"a".repeat(500),
                secret: None,
            }),
            update: None,
            disable: None,
        };
        assert!(op.validate().is_err());
    }

    #[test]
    fn test_validate_invalid_operation() {
        let op = BatchWebhookOperation {
            operation: "invalid".to_string(),
            create: None,
            update: None,
            disable: None,
        };
        assert!(op.validate().is_err());
    }
}
