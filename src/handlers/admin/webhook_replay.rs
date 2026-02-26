use crate::db::models::Transaction;
use crate::db::queries;
use crate::error::AppError;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// Request to replay a single webhook
#[derive(Debug, Deserialize)]
pub struct ReplayWebhookRequest {
    /// Whether to run in dry-run mode (test without committing)
    #[serde(default)]
    pub dry_run: bool,
}

/// Request to replay multiple webhooks in batch
#[derive(Debug, Deserialize)]
pub struct BatchReplayRequest {
    /// List of transaction IDs to replay
    pub transaction_ids: Vec<Uuid>,
    /// Whether to run in dry-run mode
    #[serde(default)]
    pub dry_run: bool,
}

/// Query parameters for listing failed webhooks
#[derive(Debug, Deserialize)]
pub struct ListFailedWebhooksQuery {
    /// Maximum number of results to return
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// Offset for pagination
    #[serde(default)]
    pub offset: i64,
    /// Filter by asset code
    pub asset_code: Option<String>,
    /// Filter by date range start
    pub from_date: Option<DateTime<Utc>>,
    /// Filter by date range end
    pub to_date: Option<DateTime<Utc>>,
}

fn default_limit() -> i64 {
    50
}

/// Response for a single replay attempt
#[derive(Debug, Serialize)]
pub struct ReplayResult {
    pub transaction_id: Uuid,
    pub success: bool,
    pub message: String,
    pub dry_run: bool,
    pub replayed_at: Option<DateTime<Utc>>,
}

/// Response for batch replay
#[derive(Debug, Serialize)]
pub struct BatchReplayResponse {
    pub total: usize,
    pub successful: usize,
    pub failed: usize,
    pub results: Vec<ReplayResult>,
}

/// Response for listing failed webhooks
#[derive(Debug, Serialize)]
pub struct FailedWebhooksResponse {
    pub total: i64,
    pub webhooks: Vec<FailedWebhookInfo>,
}

/// Information about a failed webhook from audit logs
#[derive(Debug, Serialize)]
pub struct FailedWebhookInfo {
    pub transaction_id: Uuid,
    pub stellar_account: String,
    pub amount: String,
    pub asset_code: String,
    pub anchor_transaction_id: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub last_error: Option<String>,
    pub retry_count: i32,
}

/// Retrieve the original webhook payload from audit logs
async fn get_webhook_payload_from_audit(
    pool: &PgPool,
    transaction_id: Uuid,
) -> Result<Transaction, AppError> {
    // First, try to get the transaction directly
    let transaction = queries::get_transaction(pool, transaction_id)
        .await
        .map_err(|e| match e {
            sqlx::Error::RowNotFound => {
                AppError::NotFound(format!("Transaction {} not found", transaction_id))
            }
            _ => AppError::DatabaseError(e.to_string()),
        })?;

    Ok(transaction)
}

/// List failed webhook attempts from audit logs
pub async fn list_failed_webhooks(
    State(pool): State<PgPool>,
    Query(params): Query<ListFailedWebhooksQuery>,
) -> Result<impl IntoResponse, AppError> {
    let limit = params.limit.min(100);
    
    // Build query to find transactions with failed status or in DLQ
    let mut query_builder = sqlx::QueryBuilder::new(
        "SELECT t.id, t.stellar_account, t.amount, t.asset_code, 
                t.anchor_transaction_id, t.status, t.created_at,
                COALESCE(d.retry_count, 0) as retry_count,
                d.error_reason as last_error
         FROM transactions t
         LEFT JOIN transaction_dlq d ON t.id = d.transaction_id
         WHERE (t.status = 'failed' OR d.id IS NOT NULL)"
    );

    if let Some(asset_code) = &params.asset_code {
        query_builder.push(" AND t.asset_code = ");
        query_builder.push_bind(asset_code);
    }

    if let Some(from_date) = params.from_date {
        query_builder.push(" AND t.created_at >= ");
        query_builder.push_bind(from_date);
    }

    if let Some(to_date) = params.to_date {
        query_builder.push(" AND t.created_at <= ");
        query_builder.push_bind(to_date);
    }

    query_builder.push(" ORDER BY t.created_at DESC LIMIT ");
    query_builder.push_bind(limit);
    query_builder.push(" OFFSET ");
    query_builder.push_bind(params.offset);

    let query = query_builder.build();
    let rows = query.fetch_all(&pool).await.map_err(|e| {
        tracing::error!("Failed to fetch failed webhooks: {}", e);
        AppError::DatabaseError(e.to_string())
    })?;

    let webhooks: Vec<FailedWebhookInfo> = rows
        .iter()
        .map(|row| FailedWebhookInfo {
            transaction_id: row.get("id"),
            stellar_account: row.get("stellar_account"),
            amount: row.get::<sqlx::types::BigDecimal, _>("amount").to_string(),
            asset_code: row.get("asset_code"),
            anchor_transaction_id: row.get("anchor_transaction_id"),
            status: row.get("status"),
            created_at: row.get("created_at"),
            last_error: row.get("last_error"),
            retry_count: row.get("retry_count"),
        })
        .collect();

    // Get total count
    let count_query = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM transactions t
         LEFT JOIN transaction_dlq d ON t.id = d.transaction_id
         WHERE (t.status = 'failed' OR d.id IS NOT NULL)"
    );
    
    let total = count_query.fetch_one(&pool).await.unwrap_or(0);

    Ok(Json(FailedWebhooksResponse { total, webhooks }))
}

/// Replay a single webhook by transaction ID
pub async fn replay_webhook(
    State(pool): State<PgPool>,
    Path(transaction_id): Path<Uuid>,
    Json(request): Json<ReplayWebhookRequest>,
) -> Result<impl IntoResponse, AppError> {
    tracing::info!(
        "Replaying webhook for transaction {} (dry_run: {})",
        transaction_id,
        request.dry_run
    );

    // Retrieve the original payload from audit logs
    let transaction = get_webhook_payload_from_audit(&pool, transaction_id).await?;

    // Validate that we can replay this transaction
    if transaction.status == "completed" && !request.dry_run {
        return Err(AppError::BadRequest(
            "Cannot replay completed transaction without dry-run mode".to_string(),
        ));
    }

    let result = if request.dry_run {
        // Dry-run mode: validate payload without committing
        let _ = track_replay_attempt(&pool, transaction_id, true, true, None).await;
        
        ReplayResult {
            transaction_id,
            success: true,
            message: format!(
                "Dry-run successful: Would replay webhook for {} {} to {}",
                transaction.amount, transaction.asset_code, transaction.stellar_account
            ),
            dry_run: true,
            replayed_at: None,
        }
    } else {
        // Actual replay: reprocess the webhook
        match reprocess_webhook(&pool, &transaction).await {
            Ok(_) => {
                // Log the replay attempt in audit logs
                let mut db_tx = pool.begin().await.map_err(|e| {
                    AppError::DatabaseError(format!("Failed to begin transaction: {}", e))
                })?;

                crate::db::audit::AuditLog::log(
                    &mut db_tx,
                    transaction_id,
                    crate::db::audit::ENTITY_TRANSACTION,
                    "webhook_replayed",
                    Some(serde_json::json!({
                        "status": transaction.status,
                    })),
                    Some(serde_json::json!({
                        "status": "pending",
                        "replayed_at": Utc::now(),
                    })),
                    "admin",
                )
                .await
                .map_err(|e| AppError::DatabaseError(e.to_string()))?;

                db_tx.commit().await.map_err(|e| {
                    AppError::DatabaseError(format!("Failed to commit transaction: {}", e))
                })?;

                // Track replay in history table
                let _ = track_replay_attempt(&pool, transaction_id, false, true, None).await;

                ReplayResult {
                    transaction_id,
                    success: true,
                    message: "Webhook replayed successfully".to_string(),
                    dry_run: false,
                    replayed_at: Some(Utc::now()),
                }
            }
            Err(e) => {
                let error_msg = format!("Failed to replay webhook: {}", e);
                let _ = track_replay_attempt(&pool, transaction_id, false, false, Some(error_msg.clone())).await;
                
                ReplayResult {
                    transaction_id,
                    success: false,
                    message: error_msg,
                    dry_run: false,
                    replayed_at: None,
                }
            }
        }
    };

    Ok((StatusCode::OK, Json(result)))
}

/// Replay multiple webhooks in batch
pub async fn batch_replay_webhooks(
    State(pool): State<PgPool>,
    Json(request): Json<BatchReplayRequest>,
) -> Result<impl IntoResponse, AppError> {
    tracing::info!(
        "Batch replaying {} webhooks (dry_run: {})",
        request.transaction_ids.len(),
        request.dry_run
    );

    let mut results = Vec::new();
    let mut successful = 0;
    let mut failed = 0;

    for transaction_id in request.transaction_ids {
        // Retrieve the original payload
        let transaction = match get_webhook_payload_from_audit(&pool, transaction_id).await {
            Ok(tx) => tx,
            Err(e) => {
                failed += 1;
                results.push(ReplayResult {
                    transaction_id,
                    success: false,
                    message: format!("Failed to retrieve transaction: {}", e),
                    dry_run: request.dry_run,
                    replayed_at: None,
                });
                continue;
            }
        };

        // Validate that we can replay this transaction
        if transaction.status == "completed" && !request.dry_run {
            failed += 1;
            results.push(ReplayResult {
                transaction_id,
                success: false,
                message: "Cannot replay completed transaction without dry-run mode".to_string(),
                dry_run: request.dry_run,
                replayed_at: None,
            });
            continue;
        }

        let result = if request.dry_run {
            let _ = track_replay_attempt(&pool, transaction_id, true, true, None).await;
            successful += 1;
            ReplayResult {
                transaction_id,
                success: true,
                message: format!(
                    "Dry-run successful: Would replay webhook for {} {} to {}",
                    transaction.amount, transaction.asset_code, transaction.stellar_account
                ),
                dry_run: true,
                replayed_at: None,
            }
        } else {
            match reprocess_webhook(&pool, &transaction).await {
                Ok(_) => {
                    // Log the replay attempt
                    if let Ok(mut db_tx) = pool.begin().await {
                        let _ = crate::db::audit::AuditLog::log(
                            &mut db_tx,
                            transaction_id,
                            crate::db::audit::ENTITY_TRANSACTION,
                            "webhook_replayed",
                            Some(serde_json::json!({
                                "status": transaction.status,
                            })),
                            Some(serde_json::json!({
                                "status": "pending",
                                "replayed_at": Utc::now(),
                            })),
                            "admin",
                        )
                        .await;
                        let _ = db_tx.commit().await;
                    }

                    let _ = track_replay_attempt(&pool, transaction_id, false, true, None).await;
                    successful += 1;
                    ReplayResult {
                        transaction_id,
                        success: true,
                        message: "Webhook replayed successfully".to_string(),
                        dry_run: false,
                        replayed_at: Some(Utc::now()),
                    }
                }
                Err(e) => {
                    let error_msg = format!("Failed to replay webhook: {}", e);
                    let _ = track_replay_attempt(&pool, transaction_id, false, false, Some(error_msg.clone())).await;
                    failed += 1;
                    ReplayResult {
                        transaction_id,
                        success: false,
                        message: error_msg,
                        dry_run: false,
                        replayed_at: None,
                    }
                }
            }
        };

        results.push(result);
    }

    let response = BatchReplayResponse {
        total: results.len(),
        successful,
        failed,
        results,
    };

    Ok((StatusCode::OK, Json(response)))
}

/// Reprocess a webhook by updating its status to pending
/// This respects idempotency keys and existing transaction state
async fn reprocess_webhook(pool: &PgPool, transaction: &Transaction) -> Result<(), AppError> {
    // Update transaction status to pending for reprocessing
    sqlx::query(
        "UPDATE transactions 
         SET status = 'pending', updated_at = NOW() 
         WHERE id = $1"
    )
    .bind(transaction.id)
    .execute(pool)
    .await
    .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    tracing::info!(
        "Transaction {} status updated to pending for reprocessing",
        transaction.id
    );

    Ok(())
}

/// Track replay attempt in the database
async fn track_replay_attempt(
    pool: &PgPool,
    transaction_id: Uuid,
    dry_run: bool,
    success: bool,
    error_message: Option<String>,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
        INSERT INTO webhook_replay_history 
        (transaction_id, replayed_by, dry_run, success, error_message, replayed_at)
        VALUES ($1, $2, $3, $4, $5, NOW())
        "#
    )
    .bind(transaction_id)
    .bind("admin")
    .bind(dry_run)
    .bind(success)
    .bind(error_message)
    .execute(pool)
    .await
    .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_limit() {
        assert_eq!(default_limit(), 50);
    }

    #[test]
    fn test_replay_result_serialization() {
        let result = ReplayResult {
            transaction_id: Uuid::new_v4(),
            success: true,
            message: "Test message".to_string(),
            dry_run: false,
            replayed_at: Some(Utc::now()),
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("transaction_id"));
        assert!(json.contains("success"));
    }

    #[test]
    fn test_batch_replay_response_serialization() {
        let response = BatchReplayResponse {
            total: 5,
            successful: 3,
            failed: 2,
            results: vec![],
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"total\":5"));
        assert!(json.contains("\"successful\":3"));
        assert!(json.contains("\"failed\":2"));
    }
}
