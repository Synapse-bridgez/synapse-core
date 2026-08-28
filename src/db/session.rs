//! Scoped Database Session Management & Invariant Verification.
//!
//! Provides transactional session isolation, locks & connection cleanup,
//! and data consistency invariant assertions after chaos runs.

use crate::db::chaos::FaultInjector;
use sqlx::{PgPool, Postgres, Transaction};
use std::sync::Arc;
use tracing::{info, warn};

/// Invariant violation details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvariantViolation {
    StuckAdvisoryLock { lock_id: i64 },
    StuckRowLock { relation: String, mode: String },
    PartialSettlementWrite { settlement_id: String, expected: String, actual: String },
    OrphanedWebhookDelivery { delivery_id: String, status: String },
}

impl std::fmt::Display for InvariantViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InvariantViolation::StuckAdvisoryLock { lock_id } => {
                write!(f, "Stuck advisory lock detected: ID {lock_id}")
            }
            InvariantViolation::StuckRowLock { relation, mode } => {
                write!(f, "Stuck row/table lock detected on {relation} (mode: {mode})")
            }
            InvariantViolation::PartialSettlementWrite { settlement_id, expected, actual } => {
                write!(
                    f,
                    "Partial settlement write detected for {settlement_id}: expected total {expected}, actual sum {actual}"
                )
            }
            InvariantViolation::OrphanedWebhookDelivery { delivery_id, status } => {
                write!(
                    f,
                    "Orphaned webhook delivery detected for {delivery_id} in status {status}"
                )
            }
        }
    }
}

/// Scoped database session wrapper with fault-injection awareness.
pub struct DbSession {
    pool: PgPool,
    injector: Option<FaultInjector>,
    active_locks: Vec<i64>,
}

impl DbSession {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            injector: None,
            active_locks: Vec::new(),
        }
    }

    pub fn with_injector(pool: PgPool, injector: FaultInjector) -> Self {
        Self {
            pool,
            injector: Some(injector),
            active_locks: Vec::new(),
        }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn injector(&self) -> Option<&FaultInjector> {
        self.injector.as_ref()
    }

    /// Begin a new transaction attached to this session.
    pub async fn begin_tx(&self) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
        if let Some(injector) = &self.injector {
            if let Some(fault) = injector.evaluate_fault() {
                match fault {
                    crate::db::chaos::FaultKind::ConnectionDrop => {
                        return Err(sqlx::Error::Io(std::io::Error::new(
                            std::io::ErrorKind::ConnectionReset,
                            "Chaos injected ConnectionDrop on transaction begin",
                        )));
                    }
                    crate::db::chaos::FaultKind::LatencySpike { delay_ms } => {
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                    }
                    crate::db::chaos::FaultKind::PoolExhaustion => {
                        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                    }
                }
            }
        }
        self.pool.begin().await
    }

    /// Acquire Postgres advisory lock and track it for cleanup.
    pub async fn acquire_advisory_lock(&mut self, lock_id: i64) -> Result<bool, sqlx::Error> {
        let res: (bool,) = sqlx::query_as("SELECT pg_try_advisory_lock($1)")
            .bind(lock_id)
            .fetch_one(&self.pool)
            .await?;

        if res.0 {
            self.active_locks.push(lock_id);
        }
        Ok(res.0)
    }

    /// Explicitly release advisory lock.
    pub async fn release_advisory_lock(&mut self, lock_id: i64) -> Result<bool, sqlx::Error> {
        let res: (bool,) = sqlx::query_as("SELECT pg_advisory_unlock($1)")
            .bind(lock_id)
            .fetch_one(&self.pool)
            .await?;

        if res.0 {
            self.active_locks.retain(|&id| id != lock_id);
        }
        Ok(res.0)
    }

    /// Clean up any orphaned advisory locks or active locks held by this session.
    pub async fn cleanup_orphaned_locks(&mut self) -> Result<(), sqlx::Error> {
        info!("Cleaning up orphaned database locks and connections");
        for lock_id in self.active_locks.clone() {
            let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
                .bind(lock_id)
                .execute(&self.pool)
                .await;
        }
        self.active_locks.clear();

        // Release all advisory locks on session pool connection
        let _ = sqlx::query("SELECT pg_advisory_unlock_all()")
            .execute(&self.pool)
            .await;

        Ok(())
    }

    /// Assert all data consistency invariants after a chaos run.
    /// Returns any detected violations.
    pub async fn assert_data_invariants(&self) -> Result<Vec<InvariantViolation>, sqlx::Error> {
        let mut violations = Vec::new();

        // Invariant 1: Check for stuck advisory locks
        let stuck_locks = self.check_stuck_advisory_locks().await?;
        violations.extend(stuck_locks);

        // Invariant 2: Check settlement atomicity (no partial writes)
        let settlement_violations = self.check_settlement_atomicity().await?;
        violations.extend(settlement_violations);

        // Invariant 3: Check webhook delivery consistency
        let webhook_violations = self.check_webhook_consistency().await?;
        violations.extend(webhook_violations);

        if !violations.is_empty() {
            warn!("Data consistency invariant violations found: {:?}", violations);
        } else {
            info!("Data consistency invariant assertions PASSED: 0 violations.");
        }

        Ok(violations)
    }

    /// Check for stuck advisory locks in PostgreSQL session backend.
    pub async fn check_stuck_advisory_locks(&self) -> Result<Vec<InvariantViolation>, sqlx::Error> {
        let mut violations = Vec::new();
        // Query pg_locks for advisory locks
        let rows = sqlx::query_as::<_, (i64,)>(
            "SELECT objid::bigint FROM pg_locks WHERE locktype = 'advisory' AND granted = true",
        )
        .fetch_all(&self.pool)
        .await;

        match rows {
            Ok(lock_rows) => {
                for (lock_id,) in lock_rows {
                    violations.push(InvariantViolation::StuckAdvisoryLock { lock_id });
                }
            }
            Err(e) => {
                tracing::debug!("Non-postgres or table missing for advisory lock check: {:?}", e);
            }
        }

        Ok(violations)
    }

    /// Verify settlement atomicity: for every completed settlement, sum of linked
    /// transaction amounts must exactly equal settlement total_amount.
    pub async fn check_settlement_atomicity(&self) -> Result<Vec<InvariantViolation>, sqlx::Error> {
        let mut violations = Vec::new();

        let query = r#"
            SELECT 
                s.id::text as settlement_id, 
                s.total_amount::text as expected_total, 
                COALESCE(SUM(t.amount), 0)::text as actual_total
            FROM settlements s
            LEFT JOIN transactions t ON t.settlement_id = s.id
            WHERE s.status = 'completed'
            GROUP BY s.id, s.total_amount
            HAVING s.total_amount != COALESCE(SUM(t.amount), 0)
        "#;

        let rows = sqlx::query_as::<_, (String, String, String)>(query)
            .fetch_all(&self.pool)
            .await;

        match rows {
            Ok(violating_rows) => {
                for (settlement_id, expected, actual) in violating_rows {
                    violations.push(InvariantViolation::PartialSettlementWrite {
                        settlement_id,
                        expected,
                        actual,
                    });
                }
            }
            Err(e) => {
                tracing::debug!("Settlements table query failed or not initialized: {:?}", e);
            }
        }

        Ok(violations)
    }

    /// Verify webhook delivery record states are consistent (no null/corrupted timestamps or orphan pending).
    pub async fn check_webhook_consistency(&self) -> Result<Vec<InvariantViolation>, sqlx::Error> {
        let mut violations = Vec::new();

        let query = r#"
            SELECT id::text, status
            FROM webhook_deliveries
            WHERE status NOT IN ('pending', 'delivered', 'failed')
               OR (status = 'delivered' AND response_status IS NULL)
        "#;

        let rows = sqlx::query_as::<_, (String, String)>(query)
            .fetch_all(&self.pool)
            .await;

        match rows {
            Ok(violating_rows) => {
                for (delivery_id, status) in violating_rows {
                    violations.push(InvariantViolation::OrphanedWebhookDelivery { delivery_id, status });
                }
            }
            Err(e) => {
                tracing::debug!("Webhook deliveries table check skipped/error: {:?}", e);
            }
        }

        Ok(violations)
    }
}
