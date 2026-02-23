use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    config::AppState,
    db::{
        models::{CreateTransactionRequest, Transaction, UpdateTransactionRequest},
        queries,
    },
    error::Result,
    tenant::TenantContext,
};

#[derive(Deserialize)]
pub struct PaginationParams {
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
}

fn default_limit() -> i64 {
    50
}

#[derive(Serialize)]
pub struct TransactionResponse {
    transaction: Transaction,
}

#[derive(Serialize)]
pub struct TransactionListResponse {
    transactions: Vec<Transaction>,
    total: usize,
}

pub async fn create_transaction(
    State(state): State<AppState>,
    tenant: TenantContext,
    Json(req): Json<CreateTransactionRequest>,
) -> Result<Json<TransactionResponse>> {
    let transaction = queries::create_transaction(&state.pool, tenant.tenant_id, req).await?;
    
    Ok(Json(TransactionResponse { transaction }))
}

pub async fn get_transaction(
    State(state): State<AppState>,
    tenant: TenantContext,
    Path(id): Path<Uuid>,
) -> Result<Json<TransactionResponse>> {
    let transaction = queries::get_transaction(&state.pool, tenant.tenant_id, id).await?;
    
    Ok(Json(TransactionResponse { transaction }))
}

pub async fn list_transactions(
    State(state): State<AppState>,
    tenant: TenantContext,
    Query(params): Query<PaginationParams>,
) -> Result<Json<TransactionListResponse>> {
    let transactions = queries::list_transactions(
        &state.pool,
        tenant.tenant_id,
        params.limit,
        params.offset,
    )
    .await?;
    
    let total = transactions.len();
    
    Ok(Json(TransactionListResponse {
        transactions,
        total,
    }))
}

pub async fn update_transaction(
    State(state): State<AppState>,
    tenant: TenantContext,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateTransactionRequest>,
) -> Result<Json<TransactionResponse>> {
    let transaction = queries::update_transaction(&state.pool, tenant.tenant_id, id, req).await?;
    
    Ok(Json(TransactionResponse { transaction }))
}

pub async fn delete_transaction(
    State(state): State<AppState>,
    tenant: TenantContext,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    queries::delete_transaction(&state.pool, tenant.tenant_id, id).await?;
    
    Ok(Json(serde_json::json!({ "message": "Transaction deleted" })))
}

#[derive(Deserialize)]
pub struct WebhookPayload {
    pub event_type: String,
    pub transaction_id: Option<String>,
    pub data: serde_json::Value,
}

pub async fn webhook_handler(
    State(_state): State<AppState>,
    tenant: TenantContext,
    Json(payload): Json<WebhookPayload>,
) -> Result<Json<serde_json::Value>> {
    tracing::info!(
        "Webhook received for tenant {}: {:?}",
        tenant.tenant_id,
        payload.event_type
    );
    
    Ok(Json(serde_json::json!({
        "status": "received",
        "tenant_id": tenant.tenant_id
    })))
}
