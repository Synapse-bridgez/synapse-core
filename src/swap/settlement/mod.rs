// src/swap/settlement/mod.rs
//
// Stellar settlement: durable sequence-number allocation and transaction
// submission retry for swap settlement transactions.
//
// This module provides:
//   * `SequenceAllocator` - a database-backed, replica-safe allocator of
//     monotonically increasing sequence numbers per source account, with
//     optional channel-account pooling.
//   * `RetryClassifier` - a deterministic classifier of Horizon submission
//     errors into transient (retryable with bounded backoff) and permanent
//     (terminal) outcomes, including `tx_too_low`, `tx_bad_seq`, and
//     `tx_too_late`.
//   * `SubmissionOutcome` - structured, persisted records of every submission
//     attempt for reconciliation against the swap order.
//   * Duplicate detection by transaction hash / ledger lookup so that
//     resubmission of an already-included transaction is detected rather than
//     re-applied.
//
// Security note: signed transaction envelopes are NEVER logged by this module.
// Only transaction hashes and non-sensitive metadata are emitted.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use thiserror::Error;
use tracing::{debug, info, warn};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors produced by the settlement module.
#[derive(Debug, Error)]
pub enum SettlementError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("no sequence lease available for source account {0}")]
    NoLeaseAvailable(String),

    #[error("sequence lease for source account {0} is held by another replica")]
    LeaseContended(String),

    #[error("sequence gap detected for source account {0}: expected {1}, found {2}")]
    SequenceGap(String, i64, i64),

    #[error("permanent submission failure: {0}")]
    PermanentSubmission(String),

    #[error("submission retries exhausted after {0} attempts")]
    RetriesExhausted(u32),

    #[error("transaction {0} already included in ledger {1}")]
    AlreadyIncluded(String, u32),

    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, SettlementError>;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the settlement subsystem.
#[derive(Debug, Clone)]
pub struct SettlementConfig {
    /// Maximum number of concurrent in-flight submissions per source account.
    /// Enforces per-account concurrency caps to avoid sequence churn.
    pub max_in_flight_per_account: u32,
    /// Maximum number of submission attempts before giving up on a transient
    /// error.
    pub max_submission_attempts: u32,
    /// Base backoff duration for the first retry. Subsequent retries use
    /// exponential backoff bounded by `max_backoff`.
    pub base_backoff: Duration,
    /// Upper bound on backoff between retries.
    pub max_backoff: Duration,
    /// Duration for which a sequence lease is held before it may be reclaimed
    /// by another replica (crash recovery).
    pub lease_ttl: Duration,
    /// Optional pool of channel accounts to distribute submissions across.
    /// When empty, the source account itself is used.
    pub channel_accounts: Vec<String>,
    /// Whether to enable channel-account pooling.
    pub enable_channel_pooling: bool,
}

impl Default for SettlementConfig {
    fn default() -> Self {
        Self {
            max_in_flight_per_account: 1,
            max_submission_attempts: 5,
            base_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
            lease_ttl: Duration::from_secs(60),
            channel_accounts: Vec::new(),
            enable_channel_pooling: false,
        }
    }
}

impl SettlementConfig {
    /// Validate the configuration, returning an error for invalid values.
    pub fn validate(&self) -> Result<()> {
        if self.max_in_flight_per_account == 0 {
            return Err(SettlementError::InvalidConfig(
                "max_in_flight_per_account must be >= 1".into(),
            ));
        }
        if self.max_submission_attempts == 0 {
            return Err(SettlementError::InvalidConfig(
                "max_submission_attempts must be >= 1".into(),
            ));
        }
        if self.base_backoff.is_zero() {
            return Err(SettlementError::InvalidConfig(
                "base_backoff must be > 0".into(),
            ));
        }
        if self.max_backoff < self.base_backoff {
            return Err(SettlementError::InvalidConfig(
                "max_backoff must be >= base_backoff".into(),
            ));
        }
        if self.enable_channel_pooling && self.channel_accounts.is_empty() {
            return Err(SettlementError::InvalidConfig(
                "channel pooling enabled but no channel_accounts configured".into(),
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Retry classification
// ---------------------------------------------------------------------------

/// Classification of a submission error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryDecision {
    /// Transient error; retry after the given backoff.
    Retry { backoff: Duration },
    /// Permanent error; do not retry.
    Terminal { reason: String },
    /// The transaction was already included in the ledger; treat as success.
    AlreadyIncluded { ledger: u32 },
    /// The sequence number was too low or bad; a fresh sequence must be
    /// allocated before retrying.
    ReallocateSequence { reason: String },
}

/// Deterministic classifier for Horizon submission errors.
///
/// The classifier maps Horizon `result_codes` and transport-level errors into
/// a `RetryDecision`. It is pure and deterministic given the same inputs.
#[derive(Debug, Clone)]
pub struct RetryClassifier {
    config: SettlementConfig,
}

impl RetryClassifier {
    pub fn new(config: SettlementConfig) -> Self {
        Self { config }
    }

    /// Compute the bounded exponential backoff for the given attempt number
    /// (1-indexed).
    pub fn backoff_for_attempt(&self, attempt: u32) -> Duration {
        let exp = attempt.saturating_sub(1).min(16);
        let factor = 1u64 << exp;
        let millis = self
            .config
            .base_backoff
            .as_millis()
            .saturating_mul(factor as u128)
            .min(self.config.max_backoff.as_millis());
        Duration::from_millis(millis as u64)
    }

    /// Classify a Horizon submission error.
    ///
    /// `result_code` is the top-level `result_codes.transaction` value (e.g.
    /// `"tx_bad_seq"`), and `op_codes` are the per-operation codes.
    pub fn classify(
        &self,
        attempt: u32,
        result_code: Option<&str>,
        op_codes: &[String],
        transport_error: bool,
    ) -> RetryDecision {
        // Transport-level failures (timeouts, connection resets) are
        // ambiguous: the transaction may still have been included. We treat
        // them as retryable, and the caller must perform a hash/ledger lookup
        // before resubmitting.
        if transport_error {
            return RetryDecision::Retry {
                backoff: self.backoff_for_attempt(attempt),
            };
        }

        let code = match result_code {
            Some(c) => c,
            None => {
                return RetryDecision::Retry {
                    backoff: self.backoff_for_attempt(attempt),
                }
            }
        };

        match code {
            // Sequence too low: the account's sequence has advanced past the
            // one we used. Reallocate a fresh sequence.
            "tx_too_low" => RetryDecision::ReallocateSequence {
                reason: "tx_too_low: sequence number below account sequence".into(),
            },
            // Bad sequence: mismatch between transaction and account state.
            "tx_bad_seq" => RetryDecision::ReallocateSequence {
                reason: "tx_bad_seq: sequence number mismatch".into(),
            },
            // Too late: timebounds expired. Terminal for this envelope; a new
            // envelope with fresh timebounds is required.
            "tx_too_late" => RetryDecision::Terminal {
                reason: "tx_too_late: transaction timebounds expired".into(),
            },
            // Already included / duplicate: treat as success.
            "tx_duplicate" => RetryDecision::AlreadyIncluded { ledger: 0 },
            // Insufficient fee: transient (fee strategy handled elsewhere).
            "tx_insufficient_fee" => RetryDecision::Retry {
                backoff: self.backoff_for_attempt(attempt),
            },
            // Insufficient balance: permanent until funds are added.
            "tx_insufficient_balance" => RetryDecision::Terminal {
                reason: "tx_insufficient_balance".into(),
            },
            // Malformed / bad auth / no source account: permanent.
            "tx_malformed" | "tx_bad_auth" | "tx_no_source_account" | "tx_bad_auth_extra" => {
                RetryDecision::Terminal {
                    reason: format!("permanent transaction error: {code}"),
                }
            }
            // Operation-level failures are permanent for settlement.
            "tx_failed" => {
                if op_codes.iter().any(|c| c == "op_underfunded") {
                    RetryDecision::Terminal {
                        reason: "tx_failed: op_underfunded".into(),
                    }
                } else {
                    RetryDecision::Terminal {
                        reason: format!("tx_failed: {:?}", op_codes),
                    }
                }
            }
            // Unknown codes: retry conservatively up to the attempt limit.
            other => {
                if attempt >= self.config.max_submission_attempts {
                    RetryDecision::Terminal {
                        reason: format!("unknown error after max attempts: {other}"),
                    }
                } else {
                    RetryDecision::Retry {
                        backoff: self.backoff_for_attempt(attempt),
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Submission outcome records
// ---------------------------------------------------------------------------

/// Status of a submission attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionStatus {
    /// Attempt recorded but not yet submitted.
    Pending,
    /// Submitted to Horizon and awaiting confirmation.
    Submitted,
    /// Confirmed included in a ledger.
    Confirmed,
    /// Transient failure; will be retried.
    Retryable,
    /// Permanent failure; terminal.
    Failed,
    /// Detected as already included (duplicate).
    Duplicate,
}

impl SubmissionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SubmissionStatus::Pending => "pending",
            SubmissionStatus::Submitted => "submitted",
            SubmissionStatus::Confirmed => "confirmed",
            SubmissionStatus::Retryable => "retryable",
            SubmissionStatus::Failed => "failed",
            SubmissionStatus::Duplicate => "duplicate",
        }
    }
}

/// A persisted record of a single submission attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmissionOutcome {
    pub id: Uuid,
    pub swap_order_id: Uuid,
    pub source_account: String,
    pub channel_account: Option<String>,
    pub sequence_number: i64,
    pub tx_hash: String,
    pub attempt: u32,
    pub status: SubmissionStatus,
    pub ledger: Option<u32>,
    pub result_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Repository for persisting and querying submission outcomes.
#[derive(Clone)]
pub struct SubmissionStore {
    pool: PgPool,
}

impl SubmissionStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Record a submission attempt *before* submitting, so a crash mid-submit
    /// leaves a recoverable record.
    pub async fn record_attempt(
        &self,
        swap_order_id: Uuid,
        source_account: &str,
        channel_account: Option<&str>,
        sequence_number: i64,
        tx_hash: &str,
        attempt: u32,
    ) -> Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            r#"
            INSERT INTO stellar_submission_attempts
                (id, swap_order_id, source_account, channel_account,
                 sequence_number, tx_hash, attempt, status, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW(), NOW())
            "#,
        )
        .bind(id)
        .bind(swap_order_id)
        .bind(source_account)
        .bind(channel_account)
        .bind(sequence_number)
        .bind(tx_hash)
        .bind(attempt as i32)
        .bind(SubmissionStatus::Pending.as_str())
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Update the outcome of a recorded attempt.
    pub async fn update_outcome(
        &self,
        id: Uuid,
        status: SubmissionStatus,
        ledger: Option<u32>,
        result_code: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE stellar_submission_attempts
            SET status = $2, ledger = $3, result_code = $4,
                error_message = $5, updated_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(id)
        .bind(status.as_str())
        .bind(ledger.map(|l| l as i64))
        .bind(result_code)
        .bind(error_message)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Look up an attempt by transaction hash to detect duplicates.
    pub async fn find_by_tx_hash(&self, tx_hash: &str) -> Result<Option<SubmissionOutcome>> {
        let row = sqlx::query_as::<_, SubmissionRow>(
            r#"
            SELECT id, swap_order_id, source_account, channel_account,
                   sequence_number, tx_hash, attempt, status, ledger,
                   result_code, error_message, created_at, updated_at
            FROM stellar_submission_attempts
            WHERE tx_hash = $1
            ORDER BY created_at DESC
            LIMIT 1
            "#,
        )
        .bind(tx_hash)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    /// List all attempts for a swap order, for reconciliation.
    pub async fn list_for_order(&self, swap_order_id: Uuid) -> Result<Vec<SubmissionOutcome>> {
        let rows = sqlx::query_as::<_, SubmissionRow>(
            r#"
            SELECT id, swap_order_id, source_account, channel_account,
                   sequence_number, tx_hash, attempt, status, ledger,
                   result_code, error_message, created_at, updated_at
            FROM stellar_submission_attempts
            WHERE swap_order_id = $1
            ORDER BY created_at ASC
            "#,
        )
        .bind(swap_order_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }
}

#[derive(sqlx::FromRow)]
struct SubmissionRow {
    id: Uuid,
    swap_order_id: Uuid,
    source_account: String,
    channel_account: Option<String>,
    sequence_number: i64,
    tx_hash: String,
    attempt: i32,
    status: String,
    ledger: Option<i64>,
    result_code: Option<String>,
    error_message: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<SubmissionRow> for SubmissionOutcome {
    fn from(r: SubmissionRow) -> Self {
        let status = match r.status.as_str() {
            "pending" => SubmissionStatus::Pending,
            "submitted" => SubmissionStatus::Submitted,
            "confirmed" => SubmissionStatus::Confirmed,
            "retryable" => SubmissionStatus::Retryable,
            "failed" => SubmissionStatus::Failed,
            "duplicate" => SubmissionStatus::Duplicate,
            _ => SubmissionStatus::Pending,
        };
        SubmissionOutcome {
            id: r.id,
            swap_order_id: r.swap_order_id,
            source_account: r.source_account,
            channel_account: r.channel_account,
            sequence_number: r.sequence_number,
            tx_hash: r.tx_hash,
            attempt: r.attempt as u32,
            status,
            ledger: r.ledger.map(|l| l as u32),
            result_code: r.result_code,
            error_message: r.error_message,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Durable sequence allocator
// ---------------------------------------------------------------------------

/// A leased sequence number for a source account.
#[derive(Debug, Clone)]
pub struct SequenceLease {
    pub source_account: String,
    pub channel_account: Option<String>,
    pub sequence_number: i64,
    pub lease_id: Uuid,
}

/// Database-backed sequence allocator.
///
/// Allocation is performed inside a transaction using `SELECT ... FOR UPDATE`
/// on the per-account sequence row, making it safe across multiple replicas.
/// Allocated sequences are persisted so state survives process restarts.
///
/// Gaps in allocated sequences are recoverable: the allocator tracks the
/// highest *confirmed* sequence and can re-allocate from there when a lease
/// expires without confirmation.
#[derive(Clone)]
pub struct SequenceAllocator {
    pool: PgPool,
    config: SettlementConfig,
}

impl SequenceAllocator {
    pub fn new(pool: PgPool, config: SettlementConfig) -> Self {
        Self { pool, config }
    }

    /// Select a channel account for the given source account, if pooling is
    /// enabled. Selection is deterministic (round-robin by hash) to spread
    /// load without coordination.
    pub fn select_channel_account(&self, source_account: &str) -> Option<String> {
        if !self.config.enable_channel_pooling || self.config.channel_accounts.is_empty() {
            return None;
        }
        let idx = stable_hash(source_account) % self.config.channel_accounts.len();
        Some(self.config.channel_accounts[idx].clone())
    }

    /// Allocate the next sequence number for `source_account`.
    ///
    /// This is safe across replicas: the row is locked for the duration of the
    /// transaction, the sequence is incremented, and the new value is
    /// committed. If the account has no row yet, it is initialized from
    /// `starting_sequence` (typically the account's current ledger sequence).
    pub async fn allocate(
        &self,
        source_account: &str,
        starting_sequence: i64,
    ) -> Result<SequenceLease> {
        let channel_account = self.select_channel_account(source_account);
        let mut tx = self.pool.begin().await?;

        // Ensure the row exists.
        sqlx::query(
            r#"
            INSERT INTO stellar_sequence_state
                (source_account, channel_account, next_sequence, confirmed_sequence,
                 in_flight, updated_at)
            VALUES ($1, $2, $3, $3, 0, NOW())
            ON CONFLICT (source_account, channel_account) DO NOTHING
            "#,
        )
        .bind(source_account)
        .bind(channel_account.as_deref())
        .bind(starting_sequence)
        .execute(&mut *tx)
        .await?;

        // Lock the row and read current state.
        let row = sqlx::query_as::<_, SequenceStateRow>(
            r#"
            SELECT next_sequence, confirmed_sequence, in_flight
            FROM stellar_sequence_state
            WHERE source_account = $1
              AND channel_account IS NOT DISTINCT FROM $2
            FOR UPDATE
            "#,
        )
        .bind(source_account)
        .bind(channel_account.as_deref())
        .fetch_one(&mut *tx)
        .await?;

        // Enforce per-account concurrency cap.
        if row.in_flight as u32 >= self.config.max_in_flight_per_account {
            tx.rollback().await.ok();
            return Err(SettlementError::LeaseContended(source_account.to_string()));
        }

        let allocated = row.next_sequence;
        let new_next = allocated + 1;

        sqlx::query(
            r#"
            UPDATE stellar_sequence_state
            SET next_sequence = $3, in_flight = in_flight + 1, updated_at = NOW()
            WHERE source_account = $1
              AND channel_account IS NOT DISTINCT FROM $2
            "#,
        )
        .bind(source_account)
        .bind(channel_account.as_deref())
        .bind(new_next)
        .execute(&mut *tx)
        .await?;

        let lease_id = Uuid::new_v4();
        sqlx::query(
            r#"
            INSERT INTO stellar_sequence_leases
                (lease_id, source_account, channel_account, sequence_number,
                 status, created_at, expires_at)
            VALUES ($1, $2, $3, $4, 'allocated', NOW(), NOW() + $5::interval)
            "#,
        )
        .bind(lease_id)
        .bind(source_account)
        .bind(channel_account.as_deref())
        .bind(allocated)
        .bind(format!("{} seconds", self.config.lease_ttl.as_secs()))
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        debug!(
            source_account = %source_account,
            channel_account = ?channel_account,
            sequence = allocated,
            "allocated sequence lease"
        );

        Ok(SequenceLease {
            source_account: source_account.to_string(),
            channel_account,
            sequence_number: allocated,
            lease_id,
        })
    }

    /// Mark a lease as confirmed (transaction included in `ledger`).
    pub async fn confirm(&self, lease: &SequenceLease, ledger: u32) -> Result<()> {
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            r#"
            UPDATE stellar_sequence_leases
            SET status = 'confirmed', ledger = $2, confirmed_at = NOW()
            WHERE lease_id = $1
            "#,
        )
        .bind(lease.lease_id)
        .bind(ledger as i64)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            UPDATE stellar_sequence_state
            SET confirmed_sequence = GREATEST(confirmed_sequence, $3),
                in_flight = GREATEST(in_flight - 1, 0),
                updated_at = NOW()
            WHERE source_account = $1
              AND channel_account IS NOT DISTINCT FROM $2
            "#,
        )
        .bind(&lease.source_account)
        .bind(lease.channel_account.as_deref())
        .bind(lease.sequence_number)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        info!(
            source_account = %lease.source_account,
            sequence = lease.sequence_number,
            ledger = ledger,
            "sequence lease confirmed"
        );
        Ok(())
    }

    /// Release a lease that failed permanently, freeing the in-flight slot.
    /// The sequence is *not* reused (Stellar requires monotonicity); the gap
    /// is recorded and recoverable via `recover_gaps`.
    pub async fn release(&self, lease: &SequenceLease, reason: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            r#"
            UPDATE stellar_sequence_leases
            SET status = 'released', released_at = NOW(), release_reason = $2
            WHERE lease_id = $1
            "#,
        )
        .bind(lease.lease_id)
        .bind(reason)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            UPDATE stellar_sequence_state
            SET in_flight = GREATEST(in_flight - 1, 0), updated_at = NOW()
            WHERE source_account = $1
              AND channel_account IS NOT DISTINCT FROM $2
            "#,
        )
        .bind(&lease.source_account)
        .bind(lease.channel_account.as_deref())
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        warn!(
            source_account = %lease.source_account,
            sequence = lease.sequence_number,
            reason = %reason,
            "sequence lease released (gap recorded)"
        );
        Ok(())
    }

    /// Recover from a `tx_bad_seq` / `tx_too_low` by resynchronizing the
    /// allocator's `next_sequence` with the account's actual on-chain
    /// sequence. This closes any gaps caused by expired leases.
    pub async fn resync(&self, source_account: &str, on_chain_sequence: i64) -> Result<()> {
        let channel_account = self.select_channel_account(source_account);
        sqlx::query(
            r#"
            UPDATE stellar_sequence_state
            SET next_sequence = GREATEST(next_sequence, $3),
                confirmed_sequence = GREATEST(confirmed_sequence, $3),
                updated_at = NOW()
            WHERE source_account = $1
              AND channel_account IS NOT DISTINCT FROM $2
            "#,
        )
        .bind(source_account)
        .bind(channel_account.as_deref())
        .bind(on_chain_sequence)
        .execute(&self.pool)
        .await?;
        info!(
            source_account = %source_account,
            on_chain_sequence = on_chain_sequence,
            "sequence state resynchronized"
        );
        Ok(())
    }

    /// Reclaim expired leases (crash recovery). Expired leases are marked
    /// released and their in-flight slots freed. Returns the number reclaimed.
    pub async fn reclaim_expired(&self) -> Result<u64> {
        let mut tx = self.pool.begin().await?;

        let expired = sqlx::query_as::<_, (Uuid, String, Option<String>)>(
            r#"
            SELECT lease_id, source_account, channel_account
            FROM stellar_sequence_leases
            WHERE status = 'allocated' AND expires_at < NOW()
            FOR UPDATE SKIP LOCKED
            "#,
        )
        .fetch_all(&mut *tx)
        .await?;

        for (lease_id, source_account, channel_account) in &expired {
            sqlx::query(
                r#"
                UPDATE stellar_sequence_leases
                SET status = 'expired', released_at = NOW(),
                    release_reason = 'lease expired'
                WHERE lease_id = $1
                "#,
            )
            .bind(lease_id)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                r#"
                UPDATE stellar_sequence_state
                SET in_flight = GREATEST(in_flight - 1, 0), updated_at = NOW()
                WHERE source_account = $1
                  AND channel_account IS NOT DISTINCT FROM $2
                "#,
            )
            .bind(source_account)
            .bind(channel_account.as_deref())
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        let count = expired.len() as u64;
        if count > 0 {
            warn!(count = count, "reclaimed expired sequence leases");
        }
        Ok(count)
    }
}

#[derive(sqlx::FromRow)]
struct SequenceStateRow {
    next_sequence: i64,
    confirmed_sequence: i64,
    in_flight: i64,
}

// ---------------------------------------------------------------------------
// Submission orchestration
// ---------------------------------------------------------------------------

/// Result of a submission attempt.
#[derive(Debug, Clone)]
pub enum SubmitResult {
    /// Transaction confirmed in the given ledger.
    Confirmed { ledger: u32, tx_hash: String },
    /// Transaction was already included (duplicate detected).
    AlreadyIncluded { ledger: u32, tx_hash: String },
    /// Permanent failure.
    Failed { reason: String, tx_hash: String },
}

/// Abstraction over the Horizon submission transport, so the orchestrator can
/// be tested without a live network.
#[async_trait::async_trait]
pub trait HorizonSubmitter: Send + Sync {
    /// Submit a signed envelope (base64 XDR). Implementations MUST NOT log the
    /// envelope.
    async fn submit(&self, envelope_xdr: &str) -> HorizonSubmitResponse;

    /// Look up a transaction by hash to detect duplicates.
    async fn lookup_tx(&self, tx_hash: &str) -> Option<HorizonTxLookup>;
}

/// Response from a Horizon submission.
#[derive(Debug, Clone)]
pub struct HorizonSubmitResponse {
    pub tx_hash: String,
    pub successful: bool,
    pub ledger: Option<u32>,
    pub result_code: Option<String>,
    pub op_codes: Vec<String>,
    pub transport_error: bool,
}

/// Result of a transaction hash lookup.
#[derive(Debug, Clone)]
pub struct HorizonTxLookup {
    pub tx_hash: String,
    pub ledger: u32,
    pub successful: bool,
}

/// Orchestrates submission with retry, duplicate detection, and persisted
/// outcome records.
pub struct SubmissionOrchestrator {
    allocator: SequenceAllocator,
    store: SubmissionStore,
    classifier: RetryClassifier,
    submitter: Arc<dyn HorizonSubmitter>,
    config: SettlementConfig,
}

impl SubmissionOrchestrator {
    pub fn new(
        allocator: SequenceAllocator,
        store: SubmissionStore,
        submitter: Arc<dyn HorizonSubmitter>,
        config: SettlementConfig,
    ) -> Self {
        let classifier = RetryClassifier::new(config.clone());
        Self {
            allocator,
            store,
            classifier,
            submitter,
            config,
        }
    }

    /// Submit a settlement transaction for a swap order.
    ///
    /// `build_envelope` is invoked with the allocated sequence number to
    /// produce a signed envelope. It is called again on sequence
    /// reallocation. The envelope is never logged.
    pub async fn submit_settlement<F>(
        &self,
        swap_order_id: Uuid,
        source_account: &str,
        starting_sequence: i64,
        mut build_envelope: F,
    ) -> Result<SubmitResult>
    where
        F: FnMut(i64) -> std::result::Result<(String, String), String>,
    {
        let mut attempt: u32 = 0;
        let mut current_sequence = starting_sequence;

        loop {
            attempt += 1;
            if attempt > self.config.max_submission_attempts {
                return Err(SettlementError::RetriesExhausted(
                    self.config.max_submission_attempts,
                ));
            }

            // Allocate a fresh sequence lease.
            let lease = self.allocator.allocate(source_account, current_sequence).await?;

            // Build the envelope with the allocated sequence. The envelope is
            // never logged.
            let (envelope_xdr, tx_hash) = match build_envelope(lease.sequence_number) {
                Ok(v) => v,
                Err(e) => {
                    self.allocator.release(&lease, "envelope build failed").await.ok();
                    return Err(SettlementError::Internal(format!(
                        "failed to build envelope: {e}"
                    )));
                }
            };

            // Duplicate detection: if we've seen this hash before and it was
            // confirmed, return it without resubmitting.
            if let Some(existing) = self.store.find_by_tx_hash(&tx_hash).await? {
                if existing.status == SubmissionStatus::Confirmed
                    || existing.status == SubmissionStatus::Duplicate
                {
                    if let Some(ledger) = existing.ledger {
                        self.allocator.confirm(&lease, ledger).await.ok();
                        return Ok(SubmitResult::AlreadyIncluded { ledger, tx_hash });
                    }
                }
            }

            // Record the attempt *before* submitting.
            let attempt_id = self
                .store
                .record_attempt(
                    swap_order_id,
                    source_account,
                    lease.channel_account.as_deref(),
                    lease.sequence_number,
                    &tx_hash,
                    attempt,
                )
                .await?;

            // Submit.
            let response = self.submitter.submit(&envelope_xdr).await;

            // On transport error, the transaction may still have been
            // included. Perform a hash lookup before deciding.
            if response.transport_error {
                if let Some(lookup) = self.submitter.lookup_tx(&tx_hash).await {
                    if lookup.successful {
                        self.store
                            .update_outcome(
                                attempt_id,
                                SubmissionStatus::Duplicate,
                                Some(lookup.ledger),
                                None,
                                None,
                            )
                            .await?;
                        self.allocator.confirm(&lease, lookup.ledger).await?;
                        return Ok(SubmitResult::AlreadyIncluded {
                            ledger: lookup.ledger,
                            tx_hash,
                        });
                    }
                }
            }

            // Classify the outcome.
            let decision = self.classifier.classify(
                attempt,
                response.result_code.as_deref(),
                &response.op_codes,
                response.transport_error,
            );

            match decision {
                RetryDecision::AlreadyIncluded { .. } => {
                    let ledger = response.ledger.unwrap_or(0);
                    self.store
                        .update_outcome(
                            attempt_id,
                            SubmissionStatus::Duplicate,
                            Some(ledger),
                            response.result_code.as_deref(),
                            None,
                        )
                        .await?;
                    self.allocator.confirm(&lease, ledger).await?;
                    return Ok(SubmitResult::AlreadyIncluded { ledger, tx_hash });
                }
                RetryDecision::Terminal { reason } => {
                    self.store
                        .update_outcome(
                            attempt_id,
                            SubmissionStatus::Failed,
                            response.ledger,
                            response.result_code.as_deref(),
                            Some(&reason),
                        )
                        .await?;
                    self.allocator.release(&lease, &reason).await?;
                    return Ok(SubmitResult::Failed { reason, tx_hash });
                }
                RetryDecision::ReallocateSequence { reason } => {
                    self.store
                        .update_outcome(
                            attempt_id,
                            SubmissionStatus::Retryable,
                            response.ledger,
                            response.result_code.as_deref(),
                            Some(&reason),
                        )
                        .await?;
                    self.allocator.release(&lease, &reason).await?;
                    // Resync from the response ledger if available; otherwise
                    // bump by one to escape the stale sequence.
                    let resync_to = response
                        .ledger
                        .map(|l| l as i64)
                        .unwrap_or(lease.sequence_number + 1);
                    self.allocator.resync(source_account, resync_to).await?;
                    current_sequence = resync_to;
                    continue;
                }
                RetryDecision::Retry { backoff } => {
                    self.store
                        .update_outcome(
                            attempt_id,
                            SubmissionStatus::Retryable,
                            response.ledger,
                            response.result_code.as_deref(),
                            Some("transient error"),
                        )
                        .await?;
                    self.allocator.release(&lease, "transient error").await?;
                    tokio::time::sleep(backoff).await;
                    continue;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Stable, deterministic hash for channel-account selection.
fn stable_hash(s: &str) -> usize {
    // FNV-1a 64-bit.
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash as usize
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> SettlementConfig {
        SettlementConfig {
            max_in_flight_per_account: 2,
            max_submission_attempts: 5,
            base_backoff: Duration::from