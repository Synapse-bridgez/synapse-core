use crate::db::audit::{AuditLog, ENTITY_TRANSACTION};
use crate::db::models::{Settlement, Transaction};
use crate::db::slow_query;
use crate::tenant::TenantConfig;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::types::BigDecimal;
use sqlx::{PgPool, Postgres, Result, Row, Transaction as SqlxTransaction};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::time::timeout;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Timeout tiers
// ---------------------------------------------------------------------------

/// Default read-query timeout (SELECT). Overridden by `DB_TIMEOUT_READ_SECS`.
const DEFAULT_READ_TIMEOUT_SECS: u64 = 5;
/// Default write-query timeout (INSERT/UPDATE/DELETE). Overridden by `DB_TIMEOUT_WRITE_SECS`.
const DEFAULT_WRITE_TIMEOUT_SECS: u64 = 10;
/// Default admin-query timeout (migrations, maintenance). Overridden by `DB_TIMEOUT_ADMIN_SECS`.
const DEFAULT_ADMIN_TIMEOUT_SECS: u64 = 60;

/// Global counter for timed-out queries (metric: `db_query_timeout_total`).
pub static DB_QUERY_TIMEOUT_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Tier used when wrapping a query with [`with_timeout`].
#[derive(Debug, Clone, Copy)]
pub enum QueryTier {
    Read,
    Write,
    Admin,
}

impl QueryTier {
    fn duration(self) -> Duration {
        let secs = match self {
            QueryTier::Read => std::env::var("DB_TIMEOUT_READ_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_READ_TIMEOUT_SECS),
            QueryTier::Write => std::env::var("DB_TIMEOUT_WRITE_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_WRITE_TIMEOUT_SECS),
            QueryTier::Admin => std::env::var("DB_TIMEOUT_ADMIN_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_ADMIN_TIMEOUT_SECS),
        };
        Duration::from_secs(secs)
    }

    fn label(self) -> &'static str {
        match self {
            QueryTier::Read => "read",
            QueryTier::Write => "write",
            QueryTier::Admin => "admin",
        }
    }
}

/// Wrap a database future with a timeout.
///
/// On timeout the counter `DB_QUERY_TIMEOUT_TOTAL` is incremented, the
/// sanitised SQL label is logged (no parameter values), and the connection is
/// dropped so the pool can reclaim it rather than leaving it in a hung state.
pub async fn with_timeout<F, T>(
    tier: QueryTier,
    sql_label: &str,
    fut: F,
) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    let dur = tier.duration();
    match timeout(dur, fut).await {
        Ok(result) => result,
        Err(_elapsed) => {
            DB_QUERY_TIMEOUT_TOTAL.fetch_add(1, Ordering::Relaxed);
            tracing::error!(
                tier = tier.label(),
                timeout_secs = dur.as_secs(),
                sql = sql_label,
                db_query_timeout_total = DB_QUERY_TIMEOUT_TOTAL.load(Ordering::Relaxed),
                "Database query timed out; connection will be dropped"
            );
            Err(sqlx::Error::PoolTimedOut)
        }
    }
}

// --- Tenant Queries --------------------------------------------------------

pub async fn get_all_tenant_configs(pool: &PgPool) -> Result<Vec<TenantConfig>> {
    let configs = sqlx::query_as::<_, TenantConfig>(
        "SELECT tenant_id, name, webhook_secret, stellar_account, rate_limit_per_minute, is_active FROM tenants WHERE is_active = true",
    )
    .fetch_all(pool)
    .await?;
    Ok(configs)
}

// --- Transaction Queries ---

pub async fn insert_transaction(pool: &PgPool, tx: &Transaction) -> Result<Transaction> {
    with_timeout(
        QueryTier::Write,
        "INSERT INTO transactions ... RETURNING *",
        crate::utils::retry::retry_with_backoff("insert_transaction", 3, 100, || async {
            let mut db_tx = pool.begin().await?;

            let result = sqlx::query_as::<_, Transaction>(
                r#"
            INSERT INTO transactions (
                id, stellar_account, amount, asset_code, status,
                created_at, updated_at, anchor_transaction_id, callback_type, callback_status,
                settlement_id, memo, memo_type, metadata
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
            RETURNING *
            "#,
            )
            .bind(tx.id)
            .bind(&tx.stellar_account)
            .bind(&tx.amount)
            .bind(&tx.asset_code)
            .bind(&tx.status)
            .bind(tx.created_at)
            .bind(tx.updated_at)
            .bind(&tx.anchor_transaction_id)
            .bind(&tx.callback_type)
            .bind(&tx.callback_status)
            .bind(tx.settlement_id)
            .bind(&tx.memo)
            .bind(&tx.memo_type)
            .bind(&tx.metadata)
            .fetch_one(&mut *db_tx)
            .await?;

            // Audit log: transaction created
            AuditLog::log_creation(
                &mut db_tx,
                result.id,
                ENTITY_TRANSACTION,
                json!({
                    "stellar_account": result.stellar_account,
                    "amount": result.amount.to_string(),
                    "asset_code": result.asset_code,
                    "status": result.status,
                    "anchor_transaction_id": result.anchor_transaction_id,
                    "callback_type": result.callback_type,
                    "callback_status": result.callback_status,
                    "memo": result.memo,
                    "memo_type": result.memo_type,
                    "metadata": result.metadata,
                }),
                "system",
            )
            .await?;

            db_tx.commit().await?;

            // Invalidate cache after successful commit
            invalidate_transaction_caches(&result.asset_code).await;

            Ok(result)
        }),
    )

pub async fn get_transaction(pool: &PgPool, id: Uuid) -> Result<Transaction> {
    with_timeout(
        QueryTier::Read,
        "SELECT * FROM transactions WHERE id = $1",
        sqlx::query_as::<_, Transaction>("SELECT * FROM transactions WHERE id = $1")
            .bind(id)
            .fetch_one(pool),
    )
    .await
}

pub async fn list_transactions(
    pool: &PgPool,
    limit: i64,
    cursor: Option<(DateTime<Utc>, Uuid)>,
    backward: bool,
) -> Result<Vec<Transaction>> {
    with_timeout(
        QueryTier::Read,
        "SELECT * FROM transactions [cursor-paginated]",
        async {
            if let Some((ts, id)) = cursor {
                if !backward {
                    let q = sqlx::query_as::<_, Transaction>(
                        "SELECT * FROM transactions WHERE (created_at, id) < ($1, $2) ORDER BY created_at DESC, id DESC LIMIT $3",
                    )
                    .bind(ts)
                    .bind(id)
                    .bind(limit)
                    .fetch_all(pool)
                    .await?;
                    Ok(q)
                } else {
                    let mut rows = sqlx::query_as::<_, Transaction>(
                        "SELECT * FROM transactions WHERE (created_at, id) > ($1, $2) ORDER BY created_at ASC, id ASC LIMIT $3",
                    )
                    .bind(ts)
                    .bind(id)
                    .bind(limit)
                    .fetch_all(pool)
                    .await?;
                    rows.reverse();
                    Ok(rows)
                }
            } else if !backward {
                let q = sqlx::query_as::<_, Transaction>(
                    "SELECT * FROM transactions ORDER BY created_at DESC, id DESC LIMIT $1",
                )
                .bind(limit)
                .fetch_all(pool)
                .await?;
                Ok(q)
            } else {
                let mut rows = sqlx::query_as::<_, Transaction>(
                    "SELECT * FROM transactions ORDER BY created_at ASC, id ASC LIMIT $1",
                )
                .bind(limit)
                .fetch_all(pool)
                .await?;
                rows.reverse();
                Ok(rows)
            }
        },
    )
    .await
}

pub async fn get_unsettled_transactions(
    executor: &mut SqlxTransaction<'_, Postgres>,
    asset_code: &str,
    end_time: DateTime<Utc>,
) -> Result<Vec<Transaction>> {
    with_timeout(
        QueryTier::Read,
        "SELECT * FROM transactions WHERE status = 'completed' AND settlement_id IS NULL FOR UPDATE",
        sqlx::query_as::<_, Transaction>(
            r#"
        SELECT * FROM transactions
        WHERE status = 'completed'
        AND settlement_id IS NULL
        AND asset_code = $1
        AND updated_at <= $2
        FOR UPDATE
        "#,
        )
        .bind(asset_code)
        .bind(end_time)
        .fetch_all(&mut **executor),
    )
    .await
}

pub async fn update_transactions_settlement(
    executor: &mut SqlxTransaction<'_, Postgres>,
    tx_ids: &[Uuid],
    settlement_id: Uuid,
) -> Result<()> {
    with_timeout(
        QueryTier::Write,
        "UPDATE transactions SET settlement_id = $1 WHERE id = ANY($2)",
        async {
            sqlx::query(
                "UPDATE transactions SET settlement_id = $1, updated_at = NOW() WHERE id = ANY($2)",
            )
            .bind(settlement_id)
            .bind(tx_ids)
            .execute(&mut **executor)
            .await?;

            // Audit log: record settlement_id update for each transaction
            for tx_id in tx_ids {
                AuditLog::log_field_update(
                    executor,
                    *tx_id,
                    ENTITY_TRANSACTION,
                    "settlement_id",
                    json!(null),
                    json!(settlement_id.to_string()),
                    "system",
                )
                .await?;
            }

            Ok(())
        },
    )
    .await
}

// ---------------------------------------------------------------------------
// Cache invalidation helper
// ---------------------------------------------------------------------------

async fn invalidate_transaction_caches(asset_code: &str) {
    if let Ok(redis_url) = std::env::var("REDIS_URL") {
        if let Ok(cache) = crate::services::QueryCache::new(&redis_url) {
            let _ = cache.invalidate("query:status_counts").await;
            let _ = cache.invalidate("query:daily_totals:*").await;
            let _ = cache.invalidate("query:asset_stats").await;
            let _ = cache
                .invalidate_exact(&format!("query:asset_total:{}", asset_code))
                .await;
        }
    }
}

/// Public cache invalidation function for use by other modules
pub async fn invalidate_caches_for_asset(asset_code: &str) {
    invalidate_transaction_caches(asset_code).await;
}

// ---------------------------------------------------------------------------
// Settlement Queries
// ---------------------------------------------------------------------------

pub async fn insert_settlement(
    executor: &mut SqlxTransaction<'_, Postgres>,
    settlement: &Settlement,
) -> Result<Settlement> {
    with_timeout(
        QueryTier::Write,
        "INSERT INTO settlements ... RETURNING *",
        sqlx::query_as::<_, Settlement>(
            r#"
        INSERT INTO settlements (
            id, asset_code, total_amount, tx_count, period_start, period_end, status, created_at, updated_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        RETURNING *
        "#,
        )
        .bind(settlement.id)
        .bind(&settlement.asset_code)
        .bind(&settlement.total_amount)
        .bind(settlement.tx_count)
        .bind(settlement.period_start)
        .bind(settlement.period_end)
        .bind(&settlement.status)
        .bind(settlement.created_at)
        .bind(settlement.updated_at)
        .fetch_one(&mut **executor),
    )
    .await
}

pub async fn get_settlement(pool: &PgPool, id: Uuid) -> Result<Settlement> {
    with_timeout(
        QueryTier::Read,
        "SELECT * FROM settlements WHERE id = $1",
        sqlx::query_as::<_, Settlement>("SELECT * FROM settlements WHERE id = $1")
            .bind(id)
            .fetch_one(pool),
    )
    .await
}

pub async fn list_settlements(pool: &PgPool, limit: i64, offset: i64) -> Result<Vec<Settlement>> {
    with_timeout(
        QueryTier::Read,
        "SELECT * FROM settlements ORDER BY created_at DESC LIMIT $1 OFFSET $2",
        sqlx::query_as::<_, Settlement>(
            "SELECT * FROM settlements ORDER BY created_at DESC LIMIT $1 OFFSET $2",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(pool),
    )
    .await
}

pub async fn get_unique_assets_to_settle(pool: &PgPool) -> Result<Vec<String>> {
    with_timeout(
        QueryTier::Read,
        "SELECT DISTINCT asset_code FROM transactions WHERE status = 'completed' AND settlement_id IS NULL",
        async {
            let rows = sqlx::query(
                "SELECT DISTINCT asset_code FROM transactions WHERE status = 'completed' AND settlement_id IS NULL"
            )
            .fetch_all(pool)
            .await?;

            Ok(rows
                .into_iter()
                .map(|r| r.get::<String, _>("asset_code"))
                .collect())
        },
    )
    .await
}

// ---------------------------------------------------------------------------
// Transaction Search
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub async fn search_transactions(
    pool: &PgPool,
    status: Option<&str>,
    asset_code: Option<&str>,
    min_amount: Option<&BigDecimal>,
    max_amount: Option<&BigDecimal>,
    from_date: Option<DateTime<Utc>>,
    to_date: Option<DateTime<Utc>>,
    stellar_account: Option<&str>,
    limit: i64,
    cursor: Option<(DateTime<Utc>, Uuid)>,
) -> Result<(i64, Vec<Transaction>)> {
    with_timeout(
        QueryTier::Read,
        "search_transactions [dynamic WHERE clause]",
        async {
            // Build dynamic WHERE clause
            let mut conditions = Vec::new();
            let mut param_count = 1;

            if status.is_some() {
                conditions.push(format!("status = ${}", param_count));
                param_count += 1;
            }

            if asset_code.is_some() {
                conditions.push(format!("asset_code = ${}", param_count));
                param_count += 1;
            }

            if min_amount.is_some() {
                conditions.push(format!("amount >= ${}", param_count));
                param_count += 1;
            }

            if max_amount.is_some() {
                conditions.push(format!("amount <= ${}", param_count));
                param_count += 1;
            }

            if from_date.is_some() {
                conditions.push(format!("created_at >= ${}", param_count));
                param_count += 1;
            }

            if to_date.is_some() {
                conditions.push(format!("created_at <= ${}", param_count));
                param_count += 1;
            }

            if stellar_account.is_some() {
                conditions.push(format!("stellar_account = ${}", param_count));
                param_count += 1;
            }

            // Add cursor condition
            if cursor.is_some() {
                conditions.push(format!(
                    "(created_at, id) < (${}, ${})",
                    param_count,
                    param_count + 1
                ));
                param_count += 2;
            }

            let where_clause = if conditions.is_empty() {
                String::new()
            } else {
                format!("WHERE {}", conditions.join(" AND "))
            };

            // Build count query
            let count_query = format!(
                "SELECT COUNT(*) as count FROM transactions {}",
                where_clause
            );

            // Build data query with pagination
            let data_query = format!(
                "SELECT * FROM transactions {} ORDER BY created_at DESC, id DESC LIMIT ${}",
                where_clause, param_count
            );

            // Execute count query
            let mut count_query_builder = sqlx::query(&count_query);

            if let Some(s) = status {
                count_query_builder = count_query_builder.bind(s);
            }
            if let Some(a) = asset_code {
                count_query_builder = count_query_builder.bind(a);
            }
            if let Some(min) = min_amount {
                count_query_builder = count_query_builder.bind(min);
            }
            if let Some(max) = max_amount {
                count_query_builder = count_query_builder.bind(max);
            }
            if let Some(from) = from_date {
                count_query_builder = count_query_builder.bind(from);
            }
            if let Some(to) = to_date {
                count_query_builder = count_query_builder.bind(to);
            }
            if let Some(acc) = stellar_account {
                count_query_builder = count_query_builder.bind(acc);
            }
            if let Some((ts, id)) = cursor {
                count_query_builder = count_query_builder.bind(ts).bind(id);
            }

            let count_row = count_query_builder.fetch_one(pool).await?;
            let total: i64 = count_row.try_get("count")?;

            // Execute data query
            let mut data_query_builder = sqlx::query_as::<_, Transaction>(&data_query);

            if let Some(s) = status {
                data_query_builder = data_query_builder.bind(s);
            }
            if let Some(a) = asset_code {
                data_query_builder = data_query_builder.bind(a);
            }
            if let Some(min) = min_amount {
                data_query_builder = data_query_builder.bind(min);
            }
            if let Some(max) = max_amount {
                data_query_builder = data_query_builder.bind(max);
            }
            if let Some(from) = from_date {
                data_query_builder = data_query_builder.bind(from);
            }
            if let Some(to) = to_date {
                data_query_builder = data_query_builder.bind(to);
            }
            if let Some(acc) = stellar_account {
                data_query_builder = data_query_builder.bind(acc);
            }
            if let Some((ts, id)) = cursor {
                data_query_builder = data_query_builder.bind(ts).bind(id);
            }
            data_query_builder = data_query_builder.bind(limit);

            let transactions = data_query_builder.fetch_all(pool).await?;

            Ok((total, transactions))
        },
    )
    .await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use tokio::time::Duration;

    /// Verify that `with_timeout` fires when the future exceeds the deadline.
    #[tokio::test]
    async fn test_with_timeout_triggers_on_slow_future() {
        // Override env so the timeout is 1 ms
        std::env::set_var("DB_TIMEOUT_READ_SECS", "0");

        let result = with_timeout(QueryTier::Read, "SELECT pg_sleep(10)", async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            // This line is never reached
            Err::<(), _>(sqlx::Error::RowNotFound)
        })
        .await;

        assert!(
            matches!(result, Err(sqlx::Error::PoolTimedOut)),
            "Expected PoolTimedOut, got {:?}",
            result
        );

        // Counter must have been incremented
        assert!(
            DB_QUERY_TIMEOUT_TOTAL.load(Ordering::Relaxed) >= 1,
            "Timeout counter should be >= 1"
        );

        // Restore
        std::env::remove_var("DB_TIMEOUT_READ_SECS");
    }

    /// Verify that a fast future completes without triggering the timeout.
    #[tokio::test]
    async fn test_with_timeout_passes_fast_future() {
        let before = DB_QUERY_TIMEOUT_TOTAL.load(Ordering::Relaxed);

        let result = with_timeout(QueryTier::Read, "SELECT 1", async {
            Ok::<i32, sqlx::Error>(42)
        })
        .await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(
            DB_QUERY_TIMEOUT_TOTAL.load(Ordering::Relaxed),
            before,
            "Counter should not change for a fast query"
        );
    }
}

// --- Audit Log Queries ---

/// Retrieve audit logs for a specific entity using cursor-based pagination on (timestamp, id).
pub async fn get_audit_logs(
    pool: &PgPool,
    entity_id: Uuid,
    limit: i64,
    cursor: Option<(DateTime<Utc>, Uuid)>,
) -> Result<
    Vec<(
        Uuid,
        Uuid,
        String,
        String,
        Option<serde_json::Value>,
        Option<serde_json::Value>,
        String,
        DateTime<Utc>,
    )>,
> {
    let rows = if let Some((ts, cid)) = cursor {
        sqlx::query(
            r#"
            SELECT id, entity_id, entity_type, action, old_val, new_val, actor, timestamp
            FROM audit_logs
            WHERE entity_id = $1 AND (timestamp, id) < ($2, $3)
            ORDER BY timestamp DESC, id DESC
            LIMIT $4
            "#,
        )
        .bind(entity_id)
        .bind(ts)
        .bind(cid)
        .bind(limit)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(
            r#"
            SELECT id, entity_id, entity_type, action, old_val, new_val, actor, timestamp
            FROM audit_logs
            WHERE entity_id = $1
            ORDER BY timestamp DESC, id DESC
            LIMIT $2
            "#,
        )
        .bind(entity_id)
        .bind(limit)
        .fetch_all(pool)
        .await?
    };

    Ok(rows
        .into_iter()
        .map(|row| {
            (
                row.get("id"),
                row.get("entity_id"),
                row.get("entity_type"),
                row.get("action"),
                row.get("old_val"),
                row.get("new_val"),
                row.get("actor"),
                row.get("timestamp"),
            )
        })
        .collect())
}

// --- Bulk Status Update ---

#[derive(Debug, Serialize, Deserialize)]
pub struct BulkUpdateError {
    pub transaction_id: Uuid,
    pub error: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BulkUpdateResult {
    pub updated: usize,
    pub failed: usize,
    pub errors: Vec<BulkUpdateError>,
}

/// Bulk update transaction statuses, validating each transition individually.
/// Uses a single UPDATE ... WHERE id = ANY($1) for the valid subset, then
/// audit-logs each successful update within the same transaction.
pub async fn bulk_update_transaction_status(
    pool: &PgPool,
    transaction_ids: &[Uuid],
    new_status: &str,
    reason: Option<&str>,
    actor: &str,
) -> Result<BulkUpdateResult> {
    use crate::validation::state_machine::validate_status_transition;

    // Fetch current statuses for all requested IDs in one query
    let rows = sqlx::query(
        "SELECT id, status FROM transactions WHERE id = ANY($1)",
    )
    .bind(transaction_ids)
    .fetch_all(pool)
    .await?;

    let current: std::collections::HashMap<Uuid, String> = rows
        .into_iter()
        .map(|r| (r.get::<Uuid, _>("id"), r.get::<String, _>("status")))
        .collect();

    let mut valid_ids: Vec<Uuid> = Vec::new();
    let mut old_statuses: std::collections::HashMap<Uuid, String> = std::collections::HashMap::new();
    let mut errors: Vec<BulkUpdateError> = Vec::new();

    for &id in transaction_ids {
        match current.get(&id) {
            None => errors.push(BulkUpdateError {
                transaction_id: id,
                error: "transaction not found".to_string(),
            }),
            Some(from) => match validate_status_transition(from, new_status) {
                Ok(_) => {
                    old_statuses.insert(id, from.clone());
                    valid_ids.push(id);
                }
                Err(e) => errors.push(BulkUpdateError {
                    transaction_id: id,
                    error: e.to_string(),
                }),
            },
        }
    }

    if valid_ids.is_empty() {
        return Ok(BulkUpdateResult {
            updated: 0,
            failed: errors.len(),
            errors,
        });
    }

    let mut db_tx = pool.begin().await?;

    sqlx::query(
        "UPDATE transactions SET status = $1, updated_at = NOW() WHERE id = ANY($2)",
    )
    .bind(new_status)
    .bind(&valid_ids)
    .execute(&mut *db_tx)
    .await?;

    for &id in &valid_ids {
        let old_status = old_statuses.get(&id).map(|s| s.as_str()).unwrap_or("unknown");
        let mut new_val = serde_json::json!({ "status": new_status });
        if let Some(r) = reason {
            new_val["reason"] = serde_json::json!(r);
        }
        AuditLog::log(
            &mut db_tx,
            id,
            ENTITY_TRANSACTION,
            "status_update",
            Some(serde_json::json!({ "status": old_status })),
            Some(new_val),
            actor,
        )
        .await?;
    }

    db_tx.commit().await?;

    let updated = valid_ids.len();
    Ok(BulkUpdateResult {
        updated,
        failed: errors.len(),
        errors,
    })
}

// --- Aggregate Queries (Cacheable) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusCount {
    pub status: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyTotal {
    pub date: String,
    pub total_amount: BigDecimal,
    pub tx_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetStats {
    pub asset_code: String,
    pub total_amount: BigDecimal,
    pub tx_count: i64,
    pub avg_amount: BigDecimal,
}

pub async fn get_status_counts(pool: &PgPool) -> Result<Vec<StatusCount>> {
    let rows = sqlx::query(
        r#"
        SELECT status, COUNT(*) as count
        FROM transactions
        GROUP BY status
        ORDER BY status
        "#,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| StatusCount {
            status: row.get("status"),
            count: row.get("count"),
        })
        .collect())
}

pub async fn get_daily_totals(pool: &PgPool, days: i32) -> Result<Vec<DailyTotal>> {
    let end = Utc::now();
    let start = end - chrono::Duration::days(days.into());
    let sql = r#"
        SELECT 
            DATE(created_at)::text as date,
            SUM(amount) as total_amount,
            COUNT(*) as tx_count
        FROM transactions
        WHERE created_at >= $1
          AND created_at < $2
        GROUP BY DATE(created_at)
        ORDER BY DATE(created_at) DESC
        "#;

    if cfg!(debug_assertions) {
        let explain_rows = sqlx::query(&format!("EXPLAIN ANALYZE {}", sql))
            .bind(start)
            .bind(end)
            .fetch_all(pool)
            .await?;

        let explain_plan = explain_rows
            .into_iter()
            .map(|row| row.get::<String, _>(0))
            .collect::<Vec<_>>()
            .join("\n");

        tracing::debug!("get_daily_totals EXPLAIN ANALYZE:\n{}", explain_plan);
    }

    let rows = sqlx::query(sql)
        .bind(start)
        .bind(end)
        .fetch_all(pool)
        .await?;

    Ok(rows
        .into_iter()
        .map(|row| DailyTotal {
            date: row.get("date"),
            total_amount: row.get("total_amount"),
            tx_count: row.get("tx_count"),
        })
        .collect())
}

pub async fn get_asset_stats(pool: &PgPool) -> Result<Vec<AssetStats>> {
    let rows = sqlx::query(
        r#"
        SELECT
            asset_code,
            SUM(amount) as total_amount,
            COUNT(*) as tx_count,
            AVG(amount) as avg_amount
        FROM transactions
        GROUP BY asset_code
        ORDER BY total_amount DESC
        "#,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| AssetStats {
            asset_code: row.get("asset_code"),
            total_amount: row.get("total_amount"),
            tx_count: row.get("tx_count"),
            avg_amount: row.get("avg_amount"),
        })
        .collect())
}

// --- Idempotency Fallback Queries ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdempotencyKey {
    pub key: String,
    pub status: String,
    pub response: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

pub async fn check_idempotency_key(pool: &PgPool, key: &str) -> Result<Option<IdempotencyKey>> {
    sqlx::query_as::<_, IdempotencyKey>(
        "SELECT key, status, response, created_at, expires_at FROM idempotency_keys WHERE key = $1 AND expires_at > NOW()",
    )
    .bind(key)
    .fetch_optional(pool)
    .await
}

pub async fn insert_idempotency_key(
    pool: &PgPool,
    key: &str,
    status: &str,
    response: Option<&serde_json::Value>,
    expires_at: DateTime<Utc>,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO idempotency_keys (key, status, response, expires_at)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (key) DO NOTHING
        "#,
    )
    .bind(key)
    .bind(status)
    .bind(response)
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn update_idempotency_key_response(
    pool: &PgPool,
    key: &str,
    response: &serde_json::Value,
) -> Result<()> {
    sqlx::query("UPDATE idempotency_keys SET response = $2, status = 'completed' WHERE key = $1")
        .bind(key)
        .bind(response)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn cleanup_expired_idempotency_keys(pool: &PgPool) -> Result<u64> {
    let result = sqlx::query("DELETE FROM idempotency_keys WHERE expires_at <= NOW()")
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}
