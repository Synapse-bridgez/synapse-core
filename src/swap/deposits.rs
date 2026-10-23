//! Atomic Phase 1 callback intake and Phase 2 order preparation.
use crate::db::{models::{SwapOrder, Transaction}, queries};
use crate::error::AppError;
use crate::AppState;
use sqlx::{types::BigDecimal, PgPool, Postgres, Transaction as DbTransaction};
use std::str::FromStr;
use uuid::Uuid;

/// Decimal bounds are inclusive. Asset-specific bounds apply in addition.
#[derive(Debug, Clone)]
pub struct DepositSwapConfig {
    pub min_amount: BigDecimal,
    pub max_amount: BigDecimal,
}

impl DepositSwapConfig {
    pub fn from_env() -> Result<Self, AppError> {
        let parse = |key, default: &str| {
            BigDecimal::from_str(&std::env::var(key).unwrap_or_else(|_| default.into()))
                .map_err(|_| AppError::Internal("Invalid deposit swap bounds".into()))
        };
        let config = Self {
            min_amount: parse("SWAP_DEPOSIT_MIN_AMOUNT", "1")?,
            max_amount: parse("SWAP_DEPOSIT_MAX_AMOUNT", "1000000")?,
        };
        if config.min_amount <= BigDecimal::from(0) || config.max_amount < config.min_amount {
            return Err(AppError::Internal("Invalid deposit swap bounds".into()));
        }
        Ok(config)
    }

    pub fn qualifies(&self, tx: &Transaction) -> bool {
        tx.callback_type.as_deref() == Some("deposit")
            && tx.callback_status.as_deref() == Some("completed")
            && tx.anchor_transaction_id.as_deref().is_some_and(|id| !id.is_empty())
            && tx.amount >= self.min_amount && tx.amount <= self.max_amount
    }
}

/// Internal command entry point. Tenant attribution comes exclusively from the
/// signed callback's account and persisted tenant configuration, never headers
/// or arbitrary callback metadata. Ambiguous/unattributed accounts remain Phase 1.
pub async fn persist_deposit_callback(state: &AppState, tx: &Transaction) -> Result<Transaction, AppError> {
    let tenants: Vec<Uuid> = queries::with_timeout(queries::QueryTier::Read, "resolve deposit tenant", async {
        sqlx::query_scalar("SELECT tenant_id FROM tenants WHERE stellar_account = $1 AND is_active = true LIMIT 2")
            .bind(&tx.stellar_account).fetch_all(&state.db).await
    }).await?;
    let anchor = tx.anchor_transaction_id.as_deref().filter(|id| !id.is_empty());
    let callback_type = tx.callback_type.as_deref().filter(|kind| !kind.is_empty());
    let ([tenant_id], Some(anchor), Some(kind)) = (tenants.as_slice(), anchor, callback_type) else {
        return Ok(queries::insert_transaction(&state.db, tx, Some(&state.query_cache)).await?);
    };
    // Lookup failures propagate, so a temporary flag-store failure cannot
    // acknowledge a completed deposit while silently losing its handoff.
    let enabled = state.feature_flags.is_enabled_for_tenant(super::SWAP_ENGINE_FLAG, &tenant_id.to_string()).await?;
    let config = if enabled { Some(DepositSwapConfig::from_env()?) } else { None };
    let result = queries::with_timeout(queries::QueryTier::Write, "atomic deposit callback and order", async {
        match persist_scoped(&state.db, tx, *tenant_id, anchor, kind, config.as_ref()).await {
            Err(AppError::Database(err)) if matches!(&err, sqlx::Error::Database(e)
                if e.code().as_deref() == Some("23514") && e.message().contains("no partition of relation")) => {
                queries::ensure_partition_for(&state.db, tx.created_at).await?;
                Ok(persist_scoped(&state.db, tx, *tenant_id, anchor, kind, config.as_ref()).await)
            }
            result => Ok(result),
        }
    }).await??;
    queries::invalidate_caches_for_asset(Some(&state.query_cache), &result.asset_code).await;
    Ok(result)
}

async fn persist_scoped(pool: &PgPool, incoming: &Transaction, tenant_id: Uuid, anchor: &str, kind: &str,
    config: Option<&DepositSwapConfig>) -> Result<Transaction, AppError> {
    let mut db = pool.begin().await?;
    sqlx::query("SELECT set_config('app.tenant_id', $1, true), set_config('app.is_admin', 'false', true)")
        .bind(tenant_id.to_string()).execute(&mut *db).await?;
    // A database lock serializes concurrent deliveries across service replicas;
    // unique receipt/order constraints remain the durable deduplication boundary.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(serde_json::json!([tenant_id, anchor, kind]).to_string())
        .execute(&mut *db).await?;
    let existing = sqlx::query_as::<_, Transaction>(
        "SELECT t.* FROM deposit_callback_receipts r JOIN transactions t ON t.id=r.transaction_id AND t.created_at=r.transaction_created_at AND t.tenant_id=r.tenant_id WHERE r.tenant_id=$1 AND r.anchor_transaction_id=$2 AND r.callback_type=$3 FOR UPDATE OF t")
        .bind(tenant_id).bind(anchor).bind(kind).fetch_optional(&mut *db).await?;
    let mut source = if let Some(existing) = existing {
        if existing.stellar_account != incoming.stellar_account || existing.asset_code != incoming.asset_code || existing.amount != incoming.amount {
            return Err(AppError::Validation("Callback retry changes immutable deposit fields".into()));
        }
        existing
    } else {
        let created = queries::persist_transaction_for_tenant(&mut db, incoming, Some(tenant_id)).await?;
        queries::audit_transaction_creation(&mut db, &created).await?;
        sqlx::query("INSERT INTO deposit_callback_receipts (tenant_id, anchor_transaction_id, callback_type, transaction_id, transaction_created_at) VALUES ($1,$2,$3,$4,$5)")
            .bind(tenant_id).bind(anchor).bind(kind).bind(created.id).bind(created.created_at).execute(&mut *db).await?;
        created
    };
    // A successful later callback may upgrade an earlier pending callback;
    // an old retry must never regress a successful callback or Phase 1 status.
    if incoming.callback_status.as_deref() == Some("completed") && source.callback_status.as_deref() != Some("completed") {
        crate::db::audit::AuditLog::log(&mut db, source.id, "transaction", "callback_status_update",
            Some(serde_json::json!({"callback_status": source.callback_status})),
            Some(serde_json::json!({"callback_status": "completed"})), "anchor_callback").await?;
        source = sqlx::query_as("UPDATE transactions SET callback_status='completed', updated_at=NOW() WHERE id=$1 AND created_at=$2 AND tenant_id=$3 RETURNING *")
            .bind(source.id).bind(source.created_at).bind(tenant_id).fetch_one(&mut *db).await?;
    }
    if let Some(config) = config {
        if config.qualifies(&source) {
            prepare_order(&mut db, tenant_id, &source).await?;
        }
    }
    db.commit().await?;
    Ok(source)
}

async fn prepare_order(db: &mut DbTransaction<'_, Postgres>, tenant_id: Uuid, source: &Transaction) -> Result<(), AppError> {
    // A code-only callback must resolve to one enabled asset; multiple issuers
    // are ambiguous. Read current registry data rather than a stale asset cache.
    let assets: Vec<(Option<BigDecimal>, Option<BigDecimal>)> = sqlx::query_as(
        "SELECT min_amount, max_amount FROM assets WHERE asset_code=$1 AND enabled=true LIMIT 2 FOR SHARE")
        .bind(&source.asset_code).fetch_all(&mut **db).await?;
    let [(minimum, maximum)] = assets.as_slice() else { return Ok(()); };
    if minimum.as_ref().is_some_and(|min| &source.amount < min)
        || maximum.as_ref().is_some_and(|max| &source.amount > max) { return Ok(()); }
    sqlx::query("INSERT INTO swap_orders (tenant_id, source_transaction_id, source_transaction_created_at, asset_code, amount) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (source_transaction_id) DO NOTHING")
        .bind(tenant_id).bind(source.id).bind(source.created_at).bind(&source.asset_code).bind(&source.amount)
        .execute(&mut **db).await?;
    Ok(())
}

/// Reverse linkage: Phase 1 transaction -> order, always tenant-scoped.
pub async fn order_for_transaction(pool: &PgPool, tenant_id: Uuid, source_id: Uuid) -> sqlx::Result<Option<SwapOrder>> {
    queries::with_timeout(queries::QueryTier::Read, "lookup deposit order",
        queries::with_tenant(pool, Some(tenant_id), false, |db| Box::pin(async move {
            sqlx::query_as("SELECT * FROM swap_orders WHERE tenant_id=$1 AND source_transaction_id=$2")
                .bind(tenant_id).bind(source_id).fetch_optional(&mut **db).await
        }))
    ).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deposit(amount: &str) -> Transaction {
        Transaction::new("GACCOUNT".into(), BigDecimal::from_str(amount).unwrap(), "USD".into(),
            Some("anchor-1".into()), Some("deposit".into()), Some("completed".into()), None, None, None)
    }

    #[test]
    fn qualification_uses_exact_inclusive_decimal_bounds() {
        let bounds = DepositSwapConfig { min_amount: BigDecimal::from_str("0.01").unwrap(), max_amount: BigDecimal::from(100) };
        assert!(bounds.qualifies(&deposit("0.010")));
        assert!(bounds.qualifies(&deposit("100.000")));
        assert!(!bounds.qualifies(&deposit("0.0099999999999999999999999")));
        assert!(!bounds.qualifies(&deposit("100.000000000000000000001")));
        assert!(!bounds.qualifies(&deposit("0")));
        assert!(!bounds.qualifies(&deposit("-1")));
    }

    #[test]
    fn qualification_requires_completed_identifiable_deposit() {
        let bounds = DepositSwapConfig { min_amount: BigDecimal::from(1), max_amount: BigDecimal::from(100) };
        for status in [None, Some("pending"), Some("failed"), Some("processing")] {
            let mut tx = deposit("10");
            tx.callback_status = status.map(str::to_owned);
            assert!(!bounds.qualifies(&tx));
        }
        let mut tx = deposit("10");
        tx.callback_type = Some("withdrawal".into());
        assert!(!bounds.qualifies(&tx));
        tx.callback_type = Some("deposit".into());
        tx.anchor_transaction_id = None;
        assert!(!bounds.qualifies(&tx));
    }
}
