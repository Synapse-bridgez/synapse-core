use sqlx::PgPool;
use uuid::Uuid;

use crate::db::models::{CreateTransactionRequest, Transaction, UpdateTransactionRequest};
use crate::error::Result;
use crate::tenant::TenantConfig;

pub async fn get_all_tenant_configs(pool: &PgPool) -> Result<Vec<TenantConfig>> {
    let configs = sqlx::query_as!(
        TenantConfig,
        r#"
        SELECT 
            tenant_id,
            name,
            webhook_secret,
            stellar_account,
            rate_limit_per_minute,
            is_active
        FROM tenants
        WHERE is_active = true
        "#
    )
    .fetch_all(pool)
    .await?;
    
    Ok(configs)
}

pub async fn create_transaction(
    pool: &PgPool,
    tenant_id: Uuid,
    req: CreateTransactionRequest,
) -> Result<Transaction> {
    let transaction = sqlx::query_as!(
        Transaction,
        r#"
        INSERT INTO transactions (
            transaction_id, tenant_id, external_id, status, 
            amount, asset_code, memo, created_at, updated_at
        )
        VALUES ($1, $2, $3, 'pending', $4, $5, $6, NOW(), NOW())
        RETURNING 
            transaction_id, tenant_id, external_id, status,
            amount, asset_code, stellar_transaction_id, memo,
            created_at, updated_at
        "#,
        Uuid::new_v4(),
        tenant_id,
        req.external_id,
        req.amount,
        req.asset_code,
        req.memo
    )
    .fetch_one(pool)
    .await?;
    
    Ok(transaction)
}

pub async fn get_transaction(
    pool: &PgPool,
    tenant_id: Uuid,
    transaction_id: Uuid,
) -> Result<Transaction> {
    let transaction = sqlx::query_as!(
        Transaction,
        r#"
        SELECT 
            transaction_id, tenant_id, external_id, status,
            amount, asset_code, stellar_transaction_id, memo,
            created_at, updated_at
        FROM transactions
        WHERE transaction_id = $1 AND tenant_id = $2
        "#,
        transaction_id,
        tenant_id
    )
    .fetch_optional(pool)
    .await?
    .ok_or(crate::error::AppError::TransactionNotFound)?;
    
    Ok(transaction)
}

pub async fn list_transactions(
    pool: &PgPool,
    tenant_id: Uuid,
    limit: i64,
    offset: i64,
) -> Result<Vec<Transaction>> {
    let transactions = sqlx::query_as!(
        Transaction,
        r#"
        SELECT 
            transaction_id, tenant_id, external_id, status,
            amount, asset_code, stellar_transaction_id, memo,
            created_at, updated_at
        FROM transactions
        WHERE tenant_id = $1
        ORDER BY created_at DESC
        LIMIT $2 OFFSET $3
        "#,
        tenant_id,
        limit,
        offset
    )
    .fetch_all(pool)
    .await?;
    
    Ok(transactions)
}

pub async fn update_transaction(
    pool: &PgPool,
    tenant_id: Uuid,
    transaction_id: Uuid,
    req: UpdateTransactionRequest,
) -> Result<Transaction> {
    let transaction = sqlx::query_as!(
        Transaction,
        r#"
        UPDATE transactions
        SET 
            status = COALESCE($3, status),
            stellar_transaction_id = COALESCE($4, stellar_transaction_id),
            updated_at = NOW()
        WHERE transaction_id = $1 AND tenant_id = $2
        RETURNING 
            transaction_id, tenant_id, external_id, status,
            amount, asset_code, stellar_transaction_id, memo,
            created_at, updated_at
        "#,
        transaction_id,
        tenant_id,
        req.status,
        req.stellar_transaction_id
    )
    .fetch_optional(pool)
    .await?
    .ok_or(crate::error::AppError::TransactionNotFound)?;
    
    Ok(transaction)
}

pub async fn delete_transaction(
    pool: &PgPool,
    tenant_id: Uuid,
    transaction_id: Uuid,
) -> Result<()> {
    let result = sqlx::query!(
        r#"
        DELETE FROM transactions
        WHERE transaction_id = $1 AND tenant_id = $2
        "#,
        transaction_id,
        tenant_id
    )
    .execute(pool)
    .await?;
    
    if result.rows_affected() == 0 {
        return Err(crate::error::AppError::TransactionNotFound);
    }
    
    Ok(())
}
