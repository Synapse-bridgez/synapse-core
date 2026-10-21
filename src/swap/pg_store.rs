//! Postgres-backed [`SwapStore`] (tables from
//! `migrations/20261021000001_swap_engine_store.sql`).
//!
//! Quotes and swaps are stored as JSONB documents of the engine's serde
//! types, so amounts stay decimal strings end to end.

use async_trait::async_trait;
use sqlx::PgPool;

use crate::swap::engine::{Quote, SwapEvent, SwapRecord};
use crate::swap::store::{StoreError, SwapStore};

/// The migration that creates this store's tables, for tests that set up a
/// bare database.
pub const MIGRATION_SQL: &str =
    include_str!("../../migrations/20261021000001_swap_engine_store.sql");

#[derive(Clone)]
pub struct PgSwapStore {
    pool: PgPool,
}

impl PgSwapStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create the store's tables if they don't exist (idempotent).
    pub async fn ensure_schema(&self) -> Result<(), StoreError> {
        sqlx::Executor::execute(&self.pool, MIGRATION_SQL)
            .await
            .map(|_| ())
            .map_err(backend)
    }
}

fn backend(e: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(e.to_string())
}

fn to_json<T: serde::Serialize>(v: &T) -> Result<serde_json::Value, StoreError> {
    serde_json::to_value(v).map_err(backend)
}

fn from_json<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> Result<T, StoreError> {
    serde_json::from_value(v).map_err(backend)
}

fn status_str(s: &SwapRecord) -> Result<String, StoreError> {
    match to_json(&s.status)? {
        serde_json::Value::String(s) => Ok(s),
        other => Err(backend(format!("unexpected status encoding {other}"))),
    }
}

#[async_trait]
impl SwapStore for PgSwapStore {
    async fn insert_quote(&self, quote: &Quote) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO swap_quotes (quote_id, body, created_at, expires_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(&quote.quote_id)
        .bind(to_json(quote)?)
        .bind(quote.created_at)
        .bind(quote.expires_at)
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        Ok(())
    }

    async fn get_quote(&self, quote_id: &str) -> Result<Option<Quote>, StoreError> {
        let row: Option<(serde_json::Value,)> =
            sqlx::query_as("SELECT body FROM swap_quotes WHERE quote_id = $1")
                .bind(quote_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(backend)?;
        row.map(|(b,)| from_json(b)).transpose()
    }

    async fn insert_swap(&self, swap: &SwapRecord) -> Result<(), StoreError> {
        let result = sqlx::query(
            "INSERT INTO swap_executions (swap_id, quote_id, status, body, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (quote_id) DO NOTHING",
        )
        .bind(&swap.swap_id)
        .bind(&swap.quote_id)
        .bind(status_str(swap)?)
        .bind(to_json(swap)?)
        .bind(swap.created_at)
        .bind(swap.updated_at)
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::DuplicateSwapForQuote(swap.quote_id.clone()));
        }
        Ok(())
    }

    async fn update_swap(&self, swap: &SwapRecord) -> Result<(), StoreError> {
        let result = sqlx::query(
            "UPDATE swap_executions SET status = $2, body = $3, updated_at = $4 WHERE swap_id = $1",
        )
        .bind(&swap.swap_id)
        .bind(status_str(swap)?)
        .bind(to_json(swap)?)
        .bind(swap.updated_at)
        .execute(&self.pool)
        .await
        .map_err(backend)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound(swap.swap_id.clone()));
        }
        Ok(())
    }

    async fn get_swap(&self, swap_id: &str) -> Result<Option<SwapRecord>, StoreError> {
        let row: Option<(serde_json::Value,)> =
            sqlx::query_as("SELECT body FROM swap_executions WHERE swap_id = $1")
                .bind(swap_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(backend)?;
        row.map(|(b,)| from_json(b)).transpose()
    }

    async fn get_swap_by_quote(&self, quote_id: &str) -> Result<Option<SwapRecord>, StoreError> {
        let row: Option<(serde_json::Value,)> =
            sqlx::query_as("SELECT body FROM swap_executions WHERE quote_id = $1")
                .bind(quote_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(backend)?;
        row.map(|(b,)| from_json(b)).transpose()
    }

    async fn append_event(&self, mut event: SwapEvent) -> Result<SwapEvent, StoreError> {
        let mut tx = self.pool.begin().await.map_err(backend)?;
        // Serialise appends per swap so `seq` stays gap-free.
        sqlx::query("SELECT 1 FROM swap_executions WHERE swap_id = $1 FOR UPDATE")
            .bind(&event.swap_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?
            .ok_or_else(|| StoreError::NotFound(event.swap_id.clone()))?;
        let (seq,): (i64,) = sqlx::query_as(
            "INSERT INTO swap_events (swap_id, seq, at, kind, detail) \
             SELECT $1, COALESCE(MAX(seq), 0) + 1, $2, $3, $4 FROM swap_events WHERE swap_id = $1 \
             RETURNING seq",
        )
        .bind(&event.swap_id)
        .bind(event.at)
        .bind(&event.kind)
        .bind(&event.detail)
        .fetch_one(&mut *tx)
        .await
        .map_err(backend)?;
        tx.commit().await.map_err(backend)?;
        event.seq = seq as u64;
        Ok(event)
    }

    async fn events(&self, swap_id: &str) -> Result<Vec<SwapEvent>, StoreError> {
        let rows: Vec<(i64, chrono::DateTime<chrono::Utc>, String, serde_json::Value)> =
            sqlx::query_as(
                "SELECT seq, at, kind, detail FROM swap_events WHERE swap_id = $1 ORDER BY seq",
            )
            .bind(swap_id)
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
        Ok(rows
            .into_iter()
            .map(|(seq, at, kind, detail)| SwapEvent {
                swap_id: swap_id.to_string(),
                seq: seq as u64,
                at,
                kind,
                detail,
            })
            .collect())
    }
}
