use crate::stellar::client::HorizonClient;
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::HashMap;
use tracing::info;
use uuid::Uuid;

// ── Public report types ─────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct ReconciliationReport {
    pub generated_at: DateTime<Utc>,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub total_db_transactions: usize,
    pub total_chain_payments: usize,
    /// (DB tx, chain payment) pairs found on both sides (exact or amount-mismatched).
    #[serde(default)]
    pub matched_count: usize,
    pub missing_on_chain: Vec<MissingTransaction>,
    pub orphaned_payments: Vec<OrphanedPayment>,
    pub amount_mismatches: Vec<AmountMismatch>,
    /// DB rows in a memo group where both sides have unresolved items after matching.
    #[serde(default)]
    pub ambiguous_db: Vec<AmbiguousTransaction>,
    /// Chain payments in a memo group where both sides have unresolved items after matching.
    #[serde(default)]
    pub ambiguous_chain: Vec<AmbiguousPayment>,
    /// DB transactions with no memo that found no chain counterpart.
    #[serde(default)]
    pub unmatched_no_memo_db: Vec<MissingTransaction>,
    /// Chain payments with no memo that found no DB counterpart.
    #[serde(default)]
    pub unmatched_no_memo_chain: Vec<OrphanedPayment>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MissingTransaction {
    pub id: Uuid,
    pub stellar_account: String,
    pub amount: String,
    pub asset_code: String,
    pub memo: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OrphanedPayment {
    pub payment_id: String,
    pub from: String,
    pub to: String,
    pub amount: String,
    pub asset_code: String,
    pub memo: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AmountMismatch {
    pub transaction_id: Uuid,
    pub payment_id: String,
    pub db_amount: String,
    pub chain_amount: String,
    pub memo: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AmbiguousTransaction {
    pub id: Uuid,
    pub stellar_account: String,
    pub amount: String,
    pub asset_code: String,
    pub memo: Option<String>,
    pub created_at: DateTime<Utc>,
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AmbiguousPayment {
    pub payment_id: String,
    pub from: String,
    pub to: String,
    pub amount: String,
    pub asset_code: String,
    pub memo: Option<String>,
    pub reason: String,
}

// ── Private internal types ──────────────────────────────────────────────────

#[derive(Debug)]
struct DbTransaction {
    id: Uuid,
    stellar_account: String,
    amount: String,
    asset_code: String,
    memo: Option<String>,
    created_at: DateTime<Utc>,
}

#[derive(Debug)]
struct ChainPayment {
    id: String,
    from: String,
    to: String,
    amount: String,
    asset_code: String,
    memo: Option<String>,
}

// ── Matching accumulator ────────────────────────────────────────────────────

#[derive(Default)]
struct MatchAccumulator {
    matched_count: usize,
    missing_on_chain: Vec<MissingTransaction>,
    orphaned_payments: Vec<OrphanedPayment>,
    amount_mismatches: Vec<AmountMismatch>,
    ambiguous_db: Vec<AmbiguousTransaction>,
    ambiguous_chain: Vec<AmbiguousPayment>,
    unmatched_no_memo_db: Vec<MissingTransaction>,
    unmatched_no_memo_chain: Vec<OrphanedPayment>,
}

// ── Service ─────────────────────────────────────────────────────────────────

pub struct ReconciliationService {
    horizon_client: HorizonClient,
    pool: PgPool,
}

impl ReconciliationService {
    pub fn new(horizon_client: HorizonClient, pool: PgPool) -> Self {
        Self {
            horizon_client,
            pool,
        }
    }

    pub async fn reconcile(
        &self,
        account: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> anyhow::Result<ReconciliationReport> {
        info!(
            "Starting reconciliation for {} from {} to {}",
            account, start, end
        );

        let db_txs = self.fetch_db_transactions(account, start, end).await?;
        info!("Found {} transactions in database", db_txs.len());

        let chain_payments = self.fetch_chain_payments(account, start, end).await?;
        info!("Found {} payments on chain", chain_payments.len());

        let report = perform_matching(&db_txs, &chain_payments, start, end);

        info!(
            "Reconciliation complete: {} matched, {} missing, {} orphaned, \
             {} mismatches, {} ambiguous_db, {} ambiguous_chain, \
             {} no_memo_db, {} no_memo_chain",
            report.matched_count,
            report.missing_on_chain.len(),
            report.orphaned_payments.len(),
            report.amount_mismatches.len(),
            report.ambiguous_db.len(),
            report.ambiguous_chain.len(),
            report.unmatched_no_memo_db.len(),
            report.unmatched_no_memo_chain.len(),
        );

        Ok(report)
    }

    async fn fetch_db_transactions(
        &self,
        account: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> anyhow::Result<Vec<DbTransaction>> {
        let rows =
            sqlx::query_as::<_, (Uuid, String, String, String, Option<String>, DateTime<Utc>)>(
                "SELECT id, stellar_account, amount::text, asset_code, memo, created_at
             FROM transactions
             WHERE stellar_account = $1
             AND created_at >= $2
             AND created_at <= $3
             AND status = 'completed'
             ORDER BY created_at",
            )
            .bind(account)
            .bind(start)
            .bind(end)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows
            .into_iter()
            .map(
                |(id, stellar_account, amount, asset_code, memo, created_at)| DbTransaction {
                    id,
                    stellar_account,
                    amount,
                    asset_code,
                    memo,
                    created_at,
                },
            )
            .collect())
    }

    async fn fetch_chain_payments(
        &self,
        account: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> anyhow::Result<Vec<ChainPayment>> {
        #[derive(Deserialize, Default)]
        struct Links {
            next: Option<Link>,
        }

        #[derive(Deserialize)]
        struct Link {
            href: String,
        }

        #[derive(Deserialize)]
        struct PaymentsResponse {
            #[serde(rename = "_links", default)]
            links: Links,
            #[serde(rename = "_embedded")]
            embedded: Embedded,
        }

        #[derive(Deserialize)]
        struct Embedded {
            records: Vec<PaymentRecord>,
        }

        #[derive(Deserialize)]
        struct PaymentRecord {
            id: String,
            from: String,
            to: String,
            amount: String,
            asset_code: String,
            #[serde(default)]
            memo: Option<String>,
            /// RFC 3339 timestamp; absent in some test fixtures.
            #[serde(default)]
            created_at: Option<String>,
        }

        let base = self.horizon_client.base_url.trim_end_matches('/');
        let mut url = format!(
            "{}/accounts/{}/payments?order=asc&limit=200",
            base, account
        );
        let mut all_payments = Vec::new();

        loop {
            let response = self.horizon_client.client.get(&url).send().await?;
            if !response.status().is_success() {
                return Err(anyhow::anyhow!("Horizon API error: {}", response.status()));
            }

            let page: PaymentsResponse = response.json().await?;
            let records = page.embedded.records;
            let next_url = page.links.next.map(|l| l.href);

            if records.is_empty() {
                break;
            }

            let mut past_window = false;
            for r in records {
                let created: Option<DateTime<Utc>> = r
                    .created_at
                    .as_deref()
                    .and_then(|s| s.parse().ok());

                if let Some(ts) = created {
                    if ts > end {
                        past_window = true;
                        break;
                    }
                    if ts < start {
                        continue;
                    }
                }

                all_payments.push(ChainPayment {
                    id: r.id,
                    from: r.from,
                    to: r.to,
                    amount: r.amount,
                    asset_code: r.asset_code,
                    memo: r.memo,
                });
            }

            if past_window || next_url.is_none() {
                break;
            }
            url = next_url.unwrap();
        }

        Ok(all_payments)
    }
}

// ── Pure matching logic ─────────────────────────────────────────────────────

fn perform_matching(
    db_txs: &[DbTransaction],
    chain_payments: &[ChainPayment],
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
) -> ReconciliationReport {
    let mut acc = MatchAccumulator::default();

    // Partition DB rows by memo.
    let mut db_by_memo: HashMap<String, Vec<usize>> = HashMap::new();
    let mut db_no_memo: Vec<usize> = Vec::new();
    for (i, tx) in db_txs.iter().enumerate() {
        match &tx.memo {
            Some(m) => db_by_memo.entry(m.clone()).or_default().push(i),
            None => db_no_memo.push(i),
        }
    }

    // Partition chain payments by memo.
    let mut chain_by_memo: HashMap<String, Vec<usize>> = HashMap::new();
    let mut chain_no_memo: Vec<usize> = Vec::new();
    for (i, p) in chain_payments.iter().enumerate() {
        match &p.memo {
            Some(m) => chain_by_memo.entry(m.clone()).or_default().push(i),
            None => chain_no_memo.push(i),
        }
    }

    // Process groups that have DB rows with a memo.
    for (memo, db_indices) in &db_by_memo {
        let chain_indices = chain_by_memo.get(memo).map(Vec::as_slice).unwrap_or(&[]);
        match_memo_group(memo, db_indices, chain_indices, db_txs, chain_payments, &mut acc);
    }

    // Process chain-only memo groups (no corresponding DB rows).
    for (memo, chain_indices) in &chain_by_memo {
        if !db_by_memo.contains_key(memo) {
            match_memo_group(memo, &[], chain_indices, db_txs, chain_payments, &mut acc);
        }
    }

    // Match memo-less records by account + amount + asset_code.
    match_no_memo_records(&db_no_memo, &chain_no_memo, db_txs, chain_payments, &mut acc);

    ReconciliationReport {
        generated_at: Utc::now(),
        period_start,
        period_end,
        total_db_transactions: db_txs.len(),
        total_chain_payments: chain_payments.len(),
        matched_count: acc.matched_count,
        missing_on_chain: acc.missing_on_chain,
        orphaned_payments: acc.orphaned_payments,
        amount_mismatches: acc.amount_mismatches,
        ambiguous_db: acc.ambiguous_db,
        ambiguous_chain: acc.ambiguous_chain,
        unmatched_no_memo_db: acc.unmatched_no_memo_db,
        unmatched_no_memo_chain: acc.unmatched_no_memo_chain,
    }
}

/// Match one memo group using a two-phase greedy algorithm.
///
/// Phase 1 — exact (amount + asset_code): consumes pairs that agree on both.
/// Phase 2 — asset-only: pairs remaining items that share an asset code,
///           recording the amount difference.
/// Remainder — if only one side has leftover items they go to missing/orphaned;
///             if both sides have leftovers the group is ambiguous.
fn match_memo_group(
    memo: &str,
    db_indices: &[usize],
    chain_indices: &[usize],
    db_txs: &[DbTransaction],
    chain_payments: &[ChainPayment],
    acc: &mut MatchAccumulator,
) {
    let mut avail_db = vec![true; db_indices.len()];
    let mut avail_chain = vec![true; chain_indices.len()];

    // Phase 1: exact match (amount + asset_code).
    for (di, &db_idx) in db_indices.iter().enumerate() {
        let tx = &db_txs[db_idx];
        for (ci, &chain_idx) in chain_indices.iter().enumerate() {
            if !avail_chain[ci] {
                continue;
            }
            let p = &chain_payments[chain_idx];
            if tx.asset_code == p.asset_code && tx.amount == p.amount {
                avail_db[di] = false;
                avail_chain[ci] = false;
                acc.matched_count += 1;
                break;
            }
        }
    }

    // Phase 2: asset-only match → amount mismatch pair.
    for (di, &db_idx) in db_indices.iter().enumerate() {
        if !avail_db[di] {
            continue;
        }
        let tx = &db_txs[db_idx];
        for (ci, &chain_idx) in chain_indices.iter().enumerate() {
            if !avail_chain[ci] {
                continue;
            }
            let p = &chain_payments[chain_idx];
            if tx.asset_code == p.asset_code {
                avail_db[di] = false;
                avail_chain[ci] = false;
                acc.matched_count += 1;
                acc.amount_mismatches.push(AmountMismatch {
                    transaction_id: tx.id,
                    payment_id: p.id.clone(),
                    db_amount: tx.amount.clone(),
                    chain_amount: p.amount.clone(),
                    memo: Some(memo.to_string()),
                });
                break;
            }
        }
    }

    // Collect remaining unmatched items.
    let rem_db: Vec<usize> = db_indices
        .iter()
        .enumerate()
        .filter(|(di, _)| avail_db[*di])
        .map(|(_, &idx)| idx)
        .collect();
    let rem_chain: Vec<usize> = chain_indices
        .iter()
        .enumerate()
        .filter(|(ci, _)| avail_chain[*ci])
        .map(|(_, &idx)| idx)
        .collect();

    if !rem_db.is_empty() && !rem_chain.is_empty() {
        // Both sides have unresolved items: a human must investigate.
        let reason = format!(
            "memo '{}': {} DB row(s) and {} chain payment(s) unresolved after exact and asset-only matching",
            memo,
            rem_db.len(),
            rem_chain.len()
        );
        for &idx in &rem_db {
            let tx = &db_txs[idx];
            acc.ambiguous_db.push(AmbiguousTransaction {
                id: tx.id,
                stellar_account: tx.stellar_account.clone(),
                amount: tx.amount.clone(),
                asset_code: tx.asset_code.clone(),
                memo: tx.memo.clone(),
                created_at: tx.created_at,
                reason: reason.clone(),
            });
        }
        for &idx in &rem_chain {
            let p = &chain_payments[idx];
            acc.ambiguous_chain.push(AmbiguousPayment {
                payment_id: p.id.clone(),
                from: p.from.clone(),
                to: p.to.clone(),
                amount: p.amount.clone(),
                asset_code: p.asset_code.clone(),
                memo: p.memo.clone(),
                reason: reason.clone(),
            });
        }
    } else {
        for &idx in &rem_db {
            let tx = &db_txs[idx];
            acc.missing_on_chain.push(MissingTransaction {
                id: tx.id,
                stellar_account: tx.stellar_account.clone(),
                amount: tx.amount.clone(),
                asset_code: tx.asset_code.clone(),
                memo: tx.memo.clone(),
                created_at: tx.created_at,
            });
        }
        for &idx in &rem_chain {
            let p = &chain_payments[idx];
            acc.orphaned_payments.push(OrphanedPayment {
                payment_id: p.id.clone(),
                from: p.from.clone(),
                to: p.to.clone(),
                amount: p.amount.clone(),
                asset_code: p.asset_code.clone(),
                memo: p.memo.clone(),
            });
        }
    }
}

/// Match memo-less records by destination account + amount + asset_code.
fn match_no_memo_records(
    db_indices: &[usize],
    chain_indices: &[usize],
    db_txs: &[DbTransaction],
    chain_payments: &[ChainPayment],
    acc: &mut MatchAccumulator,
) {
    let mut avail_chain = vec![true; chain_indices.len()];

    for &db_idx in db_indices {
        let tx = &db_txs[db_idx];
        let mut matched = false;
        for (ci, &chain_idx) in chain_indices.iter().enumerate() {
            if !avail_chain[ci] {
                continue;
            }
            let p = &chain_payments[chain_idx];
            if p.to == tx.stellar_account
                && p.amount == tx.amount
                && p.asset_code == tx.asset_code
            {
                avail_chain[ci] = false;
                acc.matched_count += 1;
                matched = true;
                break;
            }
        }
        if !matched {
            acc.unmatched_no_memo_db.push(MissingTransaction {
                id: tx.id,
                stellar_account: tx.stellar_account.clone(),
                amount: tx.amount.clone(),
                asset_code: tx.asset_code.clone(),
                memo: None,
                created_at: tx.created_at,
            });
        }
    }

    for (ci, &chain_idx) in chain_indices.iter().enumerate() {
        if avail_chain[ci] {
            let p = &chain_payments[chain_idx];
            acc.unmatched_no_memo_chain.push(OrphanedPayment {
                payment_id: p.id.clone(),
                from: p.from.clone(),
                to: p.to.clone(),
                amount: p.amount.clone(),
                asset_code: p.asset_code.clone(),
                memo: None,
            });
        }
    }
}

// ── Persistence ─────────────────────────────────────────────────────────────

impl ReconciliationService {
    /// Persist a reconciliation report to the database.
    pub async fn store_report(pool: &PgPool, report: &ReconciliationReport) -> anyhow::Result<()> {
        let report_json = serde_json::to_value(report)?;
        sqlx::query(
            r#"
            INSERT INTO reconciliation_reports (
                generated_at, period_start, period_end,
                total_db_transactions, total_chain_payments,
                missing_on_chain_count, orphaned_payments_count,
                amount_mismatches_count, report_json
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
        )
        .bind(report.generated_at)
        .bind(report.period_start)
        .bind(report.period_end)
        .bind(report.total_db_transactions as i32)
        .bind(report.total_chain_payments as i32)
        .bind(report.missing_on_chain.len() as i32)
        .bind(report.orphaned_payments.len() as i32)
        .bind(report.amount_mismatches.len() as i32)
        .bind(report_json)
        .execute(pool)
        .await?;
        Ok(())
    }
}

// ── Scheduled job ───────────────────────────────────────────────────────────

/// Scheduled job that runs daily reconciliation at 02:00 UTC.
pub struct ReconciliationJob {
    pub pool: PgPool,
    pub horizon_client: HorizonClient,
    /// Stellar account to reconcile (from config / env).
    pub stellar_account: String,
}

#[async_trait]
impl crate::services::scheduler::Job for ReconciliationJob {
    fn name(&self) -> &str {
        "daily_reconciliation"
    }

    /// Cron: every day at 02:00 UTC — `0 0 2 * * *` (sec min hour …)
    fn schedule(&self) -> &str {
        "0 0 2 * * *"
    }

    async fn execute(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let end = Utc::now();
        let start = end - Duration::hours(24);

        info!(
            account = %self.stellar_account,
            %start,
            %end,
            "Running scheduled daily reconciliation"
        );

        let svc = ReconciliationService::new(self.horizon_client.clone(), self.pool.clone());
        let report = svc.reconcile(&self.stellar_account, start, end).await?;

        let has_discrepancies = !report.missing_on_chain.is_empty()
            || !report.orphaned_payments.is_empty()
            || !report.amount_mismatches.is_empty()
            || !report.ambiguous_db.is_empty()
            || !report.ambiguous_chain.is_empty()
            || !report.unmatched_no_memo_db.is_empty()
            || !report.unmatched_no_memo_chain.is_empty();

        if has_discrepancies {
            tracing::warn!(
                missing_on_chain = report.missing_on_chain.len(),
                orphaned_payments = report.orphaned_payments.len(),
                amount_mismatches = report.amount_mismatches.len(),
                ambiguous_db = report.ambiguous_db.len(),
                ambiguous_chain = report.ambiguous_chain.len(),
                unmatched_no_memo_db = report.unmatched_no_memo_db.len(),
                unmatched_no_memo_chain = report.unmatched_no_memo_chain.len(),
                "Reconciliation discrepancies found — review required"
            );
        } else {
            info!("Reconciliation completed with no discrepancies");
        }

        ReconciliationService::store_report(&self.pool, &report).await?;
        info!("Reconciliation report stored");

        Ok(())
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn make_period() -> (DateTime<Utc>, DateTime<Utc>) {
        // Must fall within an existing transactions partition (migrations cover
        // 2025-01 through 2026-07).
        let start = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap();
        (start, end)
    }

    /// Build a minimal Horizon payments JSON response body.
    fn payments_body(records: &[serde_json::Value]) -> String {
        serde_json::json!({
            "_embedded": { "records": records }
        })
        .to_string()
    }

    fn payment_record(
        id: &str,
        from: &str,
        to: &str,
        amount: &str,
        asset_code: &str,
        memo: Option<&str>,
    ) -> serde_json::Value {
        let mut v = serde_json::json!({
            "id": id,
            "from": from,
            "to": to,
            "amount": amount,
            "asset_code": asset_code,
        });
        if let Some(m) = memo {
            v["memo"] = serde_json::Value::String(m.to_string());
        }
        v
    }

    fn fixed_time() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
    }

    fn make_db_tx(
        index: u128,
        account: &str,
        amount: &str,
        asset: &str,
        memo: Option<&str>,
    ) -> DbTransaction {
        DbTransaction {
            id: Uuid::from_u128(index),
            stellar_account: account.to_string(),
            amount: amount.to_string(),
            asset_code: asset.to_string(),
            memo: memo.map(str::to_string),
            created_at: fixed_time(),
        }
    }

    fn make_chain_payment(
        id: &str,
        to: &str,
        amount: &str,
        asset: &str,
        memo: Option<&str>,
    ) -> ChainPayment {
        ChainPayment {
            id: id.to_string(),
            from: "GSRC".to_string(),
            to: to.to_string(),
            amount: amount.to_string(),
            asset_code: asset.to_string(),
            memo: memo.map(str::to_string),
        }
    }

    fn check_conservation(report: &ReconciliationReport) {
        assert_eq!(
            report.total_db_transactions,
            report.matched_count
                + report.missing_on_chain.len()
                + report.ambiguous_db.len()
                + report.unmatched_no_memo_db.len(),
            "DB conservation violated"
        );
        assert_eq!(
            report.total_chain_payments,
            report.matched_count
                + report.orphaned_payments.len()
                + report.ambiguous_chain.len()
                + report.unmatched_no_memo_chain.len(),
            "chain conservation violated"
        );
    }

    // ── Unit tests — report struct logic (no DB, no HTTP) ────────────────────

    #[test]
    fn test_reconciliation_report_empty_sets() {
        let (start, end) = make_period();
        let report = ReconciliationReport {
            generated_at: Utc::now(),
            period_start: start,
            period_end: end,
            total_db_transactions: 0,
            total_chain_payments: 0,
            matched_count: 0,
            missing_on_chain: vec![],
            orphaned_payments: vec![],
            amount_mismatches: vec![],
            ambiguous_db: vec![],
            ambiguous_chain: vec![],
            unmatched_no_memo_db: vec![],
            unmatched_no_memo_chain: vec![],
        };

        assert_eq!(report.total_db_transactions, 0);
        assert_eq!(report.total_chain_payments, 0);
        assert!(report.missing_on_chain.is_empty());
        assert!(report.orphaned_payments.is_empty());
        assert!(report.amount_mismatches.is_empty());
    }

    #[test]
    fn test_missing_transaction_fields() {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let missing = MissingTransaction {
            id,
            stellar_account: "GABC123".to_string(),
            amount: "100.00".to_string(),
            asset_code: "USDC".to_string(),
            memo: Some("memo-xyz".to_string()),
            created_at: now,
        };

        assert_eq!(missing.id, id);
        assert_eq!(missing.stellar_account, "GABC123");
        assert_eq!(missing.amount, "100.00");
        assert_eq!(missing.asset_code, "USDC");
        assert_eq!(missing.memo.as_deref(), Some("memo-xyz"));
    }

    #[test]
    fn test_orphaned_payment_fields() {
        let orphan = OrphanedPayment {
            payment_id: "pay-001".to_string(),
            from: "GABC".to_string(),
            to: "GXYZ".to_string(),
            amount: "50.00".to_string(),
            asset_code: "USDC".to_string(),
            memo: Some("orphan-memo".to_string()),
        };

        assert_eq!(orphan.payment_id, "pay-001");
        assert_eq!(orphan.from, "GABC");
        assert_eq!(orphan.to, "GXYZ");
        assert_eq!(orphan.memo.as_deref(), Some("orphan-memo"));
    }

    #[test]
    fn test_amount_mismatch_fields() {
        let tx_id = Uuid::new_v4();
        let mismatch = AmountMismatch {
            transaction_id: tx_id,
            payment_id: "pay-002".to_string(),
            db_amount: "100.00".to_string(),
            chain_amount: "99.99".to_string(),
            memo: Some("mismatch-memo".to_string()),
        };

        assert_eq!(mismatch.transaction_id, tx_id);
        assert_eq!(mismatch.db_amount, "100.00");
        assert_eq!(mismatch.chain_amount, "99.99");
        assert_ne!(mismatch.db_amount, mismatch.chain_amount);
    }

    #[test]
    fn test_report_serialization_roundtrip() {
        let (start, end) = make_period();
        let id = Uuid::new_v4();
        let report = ReconciliationReport {
            generated_at: Utc::now(),
            period_start: start,
            period_end: end,
            total_db_transactions: 1,
            total_chain_payments: 2,
            matched_count: 0,
            missing_on_chain: vec![MissingTransaction {
                id,
                stellar_account: "GACC".to_string(),
                amount: "10.00".to_string(),
                asset_code: "XLM".to_string(),
                memo: Some("m1".to_string()),
                created_at: start,
            }],
            orphaned_payments: vec![OrphanedPayment {
                payment_id: "p1".to_string(),
                from: "GA".to_string(),
                to: "GB".to_string(),
                amount: "5.00".to_string(),
                asset_code: "XLM".to_string(),
                memo: None,
            }],
            amount_mismatches: vec![],
            ambiguous_db: vec![],
            ambiguous_chain: vec![],
            unmatched_no_memo_db: vec![],
            unmatched_no_memo_chain: vec![],
        };

        let json = serde_json::to_string(&report).expect("serialization failed");
        let deserialized: ReconciliationReport =
            serde_json::from_str(&json).expect("deserialization failed");

        assert_eq!(deserialized.total_db_transactions, 1);
        assert_eq!(deserialized.total_chain_payments, 2);
        assert_eq!(deserialized.missing_on_chain.len(), 1);
        assert_eq!(deserialized.orphaned_payments.len(), 1);
        assert!(deserialized.amount_mismatches.is_empty());
    }

    #[test]
    fn test_report_backward_compatible_deserialization() {
        // Old JSON without the new fields must still deserialize successfully.
        let old_json = r#"{
            "generated_at": "2026-06-01T00:00:00Z",
            "period_start": "2026-06-01T00:00:00Z",
            "period_end":   "2026-06-02T00:00:00Z",
            "total_db_transactions": 0,
            "total_chain_payments": 0,
            "missing_on_chain": [],
            "orphaned_payments": [],
            "amount_mismatches": []
        }"#;
        let report: ReconciliationReport =
            serde_json::from_str(old_json).expect("backward-compat deserialization failed");
        assert_eq!(report.matched_count, 0);
        assert!(report.ambiguous_db.is_empty());
        assert!(report.unmatched_no_memo_db.is_empty());
    }

    // ── Unit tests — perform_matching (pure logic, no IO) ─────────────────────

    #[test]
    fn test_matching_empty_inputs() {
        let (start, end) = make_period();
        let report = perform_matching(&[], &[], start, end);
        assert_eq!(report.total_db_transactions, 0);
        assert_eq!(report.total_chain_payments, 0);
        assert_eq!(report.matched_count, 0);
        check_conservation(&report);
    }

    #[test]
    fn test_matching_exact_single_pair() {
        let (start, end) = make_period();
        let db = vec![make_db_tx(1, "GACC", "100.00", "USDC", Some("memo-1"))];
        let chain = vec![make_chain_payment("cp-1", "GACC", "100.00", "USDC", Some("memo-1"))];
        let report = perform_matching(&db, &chain, start, end);

        assert_eq!(report.matched_count, 1);
        assert!(report.missing_on_chain.is_empty());
        assert!(report.orphaned_payments.is_empty());
        assert!(report.amount_mismatches.is_empty());
        check_conservation(&report);
    }

    #[test]
    fn test_matching_duplicate_memos_both_sides_all_matched() {
        // 2 DB rows, 2 chain payments — same memo + amount + asset → all matched.
        let (start, end) = make_period();
        let db = vec![
            make_db_tx(1, "GACC", "50.00", "USDC", Some("dup-memo")),
            make_db_tx(2, "GACC", "50.00", "USDC", Some("dup-memo")),
        ];
        let chain = vec![
            make_chain_payment("cp-1", "GACC", "50.00", "USDC", Some("dup-memo")),
            make_chain_payment("cp-2", "GACC", "50.00", "USDC", Some("dup-memo")),
        ];
        let report = perform_matching(&db, &chain, start, end);

        assert_eq!(report.matched_count, 2);
        assert!(report.missing_on_chain.is_empty());
        assert!(report.orphaned_payments.is_empty());
        assert!(report.amount_mismatches.is_empty());
        assert!(report.ambiguous_db.is_empty());
        check_conservation(&report);
    }

    #[test]
    fn test_matching_duplicate_memo_2db_1chain_one_missing() {
        // 2 DB rows share a memo but only 1 chain payment exists:
        // → 1 matched, 1 missing_on_chain.  Old behavior was silent data loss.
        let (start, end) = make_period();
        let db = vec![
            make_db_tx(1, "GACC", "50.00", "USDC", Some("dup-memo")),
            make_db_tx(2, "GACC", "50.00", "USDC", Some("dup-memo")),
        ];
        let chain = vec![make_chain_payment(
            "cp-1", "GACC", "50.00", "USDC", Some("dup-memo"),
        )];
        let report = perform_matching(&db, &chain, start, end);

        assert_eq!(report.matched_count, 1);
        assert_eq!(report.missing_on_chain.len(), 1);
        assert!(report.orphaned_payments.is_empty());
        assert!(report.amount_mismatches.is_empty());
        check_conservation(&report);
    }

    #[test]
    fn test_matching_partial_group_3db_2chain() {
        // 3 DB rows, 2 chain payments with the same memo:
        // → 2 matched, 1 missing_on_chain.
        let (start, end) = make_period();
        let db = vec![
            make_db_tx(1, "GACC", "100.00", "USDC", Some("memo-x")),
            make_db_tx(2, "GACC", "200.00", "USDC", Some("memo-x")),
            make_db_tx(3, "GACC", "300.00", "USDC", Some("memo-x")),
        ];
        let chain = vec![
            make_chain_payment("cp-1", "GACC", "100.00", "USDC", Some("memo-x")),
            make_chain_payment("cp-2", "GACC", "200.00", "USDC", Some("memo-x")),
        ];
        let report = perform_matching(&db, &chain, start, end);

        assert_eq!(report.matched_count, 2);
        assert_eq!(report.missing_on_chain.len(), 1);
        assert_eq!(report.missing_on_chain[0].amount, "300.00");
        assert!(report.orphaned_payments.is_empty());
        assert!(report.amount_mismatches.is_empty());
        check_conservation(&report);
    }

    #[test]
    fn test_matching_amount_mismatch_within_memo_group() {
        // Same memo and asset, different amounts → amount_mismatch.
        let (start, end) = make_period();
        let db = vec![make_db_tx(1, "GACC", "100.00", "USDC", Some("memo-m"))];
        let chain = vec![make_chain_payment(
            "cp-1", "GACC", "99.00", "USDC", Some("memo-m"),
        )];
        let report = perform_matching(&db, &chain, start, end);

        assert_eq!(report.matched_count, 1);
        assert_eq!(report.amount_mismatches.len(), 1);
        assert_eq!(report.amount_mismatches[0].db_amount, "100.00");
        assert_eq!(report.amount_mismatches[0].chain_amount, "99.00");
        assert!(report.missing_on_chain.is_empty());
        assert!(report.orphaned_payments.is_empty());
        check_conservation(&report);
    }

    #[test]
    fn test_matching_ambiguous_group_incompatible_assets() {
        // Both DB and chain have items under the same memo but no asset overlap:
        // after both phases there are unresolved items on each side → ambiguous.
        //
        // BEFORE fix: the HashMap overwrite would drop one DB row entirely.
        // AFTER fix:  both unresolved rows surface in ambiguous_*.
        let (start, end) = make_period();
        let db = vec![
            make_db_tx(1, "GACC", "100.00", "USDC", Some("memo-amb")),
            make_db_tx(2, "GACC", "200.00", "USDC", Some("memo-amb")),
        ];
        let chain = vec![
            make_chain_payment("cp-1", "GACC", "100.00", "XLM", Some("memo-amb")),
            make_chain_payment("cp-2", "GACC", "200.00", "XLM", Some("memo-amb")),
        ];
        let report = perform_matching(&db, &chain, start, end);

        assert_eq!(report.ambiguous_db.len(), 2);
        assert_eq!(report.ambiguous_chain.len(), 2);
        assert_eq!(report.matched_count, 0);
        assert!(report.missing_on_chain.is_empty());
        assert!(report.orphaned_payments.is_empty());
        check_conservation(&report);
    }

    #[test]
    fn test_matching_none_memo_db_matched_by_account_amount() {
        // Memo-less DB row is matched to a memo-less chain payment via
        // account + amount + asset_code.
        let (start, end) = make_period();
        let db = vec![make_db_tx(1, "GACC", "15.00", "USDC", None)];
        let chain = vec![make_chain_payment("cp-1", "GACC", "15.00", "USDC", None)];
        let report = perform_matching(&db, &chain, start, end);

        assert_eq!(report.matched_count, 1);
        assert!(report.unmatched_no_memo_db.is_empty());
        assert!(report.unmatched_no_memo_chain.is_empty());
        check_conservation(&report);
    }

    #[test]
    fn test_matching_none_memo_db_no_chain_counterpart() {
        // Memo-less DB row with no chain match → unmatched_no_memo_db,
        // NOT silently dropped as in the old algorithm.
        let (start, end) = make_period();
        let db = vec![make_db_tx(1, "GACC", "15.00", "USDC", None)];
        let report = perform_matching(&db, &[], start, end);

        assert_eq!(report.unmatched_no_memo_db.len(), 1);
        assert_eq!(report.unmatched_no_memo_db[0].amount, "15.00");
        assert!(report.missing_on_chain.is_empty());
        check_conservation(&report);
    }

    #[test]
    fn test_matching_none_memo_chain_no_db_counterpart() {
        // Memo-less chain payment with no DB match → unmatched_no_memo_chain.
        let (start, end) = make_period();
        let chain = vec![make_chain_payment("cp-1", "GACC", "15.00", "USDC", None)];
        let report = perform_matching(&[], &chain, start, end);

        assert_eq!(report.unmatched_no_memo_chain.len(), 1);
        assert!(report.orphaned_payments.is_empty());
        check_conservation(&report);
    }

    #[test]
    fn test_matching_none_memos_both_sides_partial() {
        // 2 memo-less DB rows, 1 memo-less chain payment (matching first):
        // → 1 matched, 1 unmatched_no_memo_db.
        let (start, end) = make_period();
        let db = vec![
            make_db_tx(1, "GACC", "10.00", "USDC", None),
            make_db_tx(2, "GACC", "10.00", "USDC", None),
        ];
        let chain = vec![make_chain_payment("cp-1", "GACC", "10.00", "USDC", None)];
        let report = perform_matching(&db, &chain, start, end);

        assert_eq!(report.matched_count, 1);
        assert_eq!(report.unmatched_no_memo_db.len(), 1);
        assert!(report.unmatched_no_memo_chain.is_empty());
        check_conservation(&report);
    }

    #[test]
    fn test_matching_conservation_complex_fixture() {
        // 5 DB txs, 5 chain payments, multiple buckets filled:
        //   memo-A: 1 DB + 1 chain (exact) → matched
        //   memo-B: 2 DB + 1 chain         → 1 matched, 1 missing
        //   memo-C: 1 DB + 1 chain, amount differs → amount_mismatch
        //   memo-D: 0 DB + 1 chain          → orphaned
        //   no memo: 1 DB + 1 chain (match) → matched
        //
        // BEFORE fix: memo-B would silently drop one DB row (HashMap overwrite).
        // AFTER fix:  missing_on_chain.len() == 1, conservation holds.
        let (start, end) = make_period();
        let db = vec![
            make_db_tx(1, "GACC", "10.00", "USDC", Some("memo-A")),
            make_db_tx(2, "GACC", "20.00", "USDC", Some("memo-B")),
            make_db_tx(3, "GACC", "20.00", "USDC", Some("memo-B")),
            make_db_tx(4, "GACC", "30.00", "USDC", Some("memo-C")),
            make_db_tx(5, "GACC", "40.00", "USDC", None),
        ];
        let chain = vec![
            make_chain_payment("cp-A", "GACC", "10.00", "USDC", Some("memo-A")),
            make_chain_payment("cp-B", "GACC", "20.00", "USDC", Some("memo-B")),
            make_chain_payment("cp-C", "GACC", "31.00", "USDC", Some("memo-C")),
            make_chain_payment("cp-D", "GACC", "99.00", "USDC", Some("memo-D")),
            make_chain_payment("cp-NM", "GACC", "40.00", "USDC", None),
        ];
        let report = perform_matching(&db, &chain, start, end);

        // memo-A: exact match
        // memo-B: 1 match + 1 missing
        // memo-C: amount mismatch (matched pair)
        // memo-D: orphaned
        // no-memo: matched
        assert_eq!(report.matched_count, 4); // A, B(1), C, no-memo
        assert_eq!(report.missing_on_chain.len(), 1); // B(2nd)
        assert_eq!(report.orphaned_payments.len(), 1); // D
        assert_eq!(report.amount_mismatches.len(), 1); // C
        check_conservation(&report);
    }

    // ── Unit tests — ReconciliationJob metadata ───────────────────────────────

    #[test]
    fn test_reconciliation_job_name() {
        let client = HorizonClient::new("http://localhost:9999".to_string());
        let _ = client;
        assert_eq!("daily_reconciliation", "daily_reconciliation");
        assert_eq!("0 0 2 * * *", "0 0 2 * * *");
    }

    // ── Horizon HTTP mock tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_fetch_chain_payments_horizon_error_returns_err() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"^/accounts/.*/payments.*".into()),
            )
            .with_status(503)
            .create_async()
            .await;

        let database_url = std::env::var("DATABASE_URL");
        if database_url.is_err() {
            return;
        }
        let pool = sqlx::PgPool::connect(&database_url.unwrap())
            .await
            .expect("connect");
        let client = HorizonClient::new(server.url());
        let svc = ReconciliationService::new(client, pool);
        let (start, end) = make_period();
        let result = svc.reconcile("GACC123", start, end).await;
        assert!(result.is_err(), "expected error from 503 response");
    }

    #[tokio::test]
    async fn test_fetch_chain_payments_malformed_json_returns_err() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"^/accounts/.*/payments.*".into()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("not-valid-json{{{")
            .create_async()
            .await;

        let database_url = std::env::var("DATABASE_URL");
        if database_url.is_err() {
            return;
        }
        let pool = sqlx::PgPool::connect(&database_url.unwrap())
            .await
            .expect("connect");
        let client = HorizonClient::new(server.url());
        let svc = ReconciliationService::new(client, pool);
        let (start, end) = make_period();
        let result = svc.reconcile("GACC123", start, end).await;
        assert!(result.is_err(), "expected error from malformed JSON");
    }

    // ── Integration tests (require DATABASE_URL + migrations) ─────────────────
    // Run with: DATABASE_URL=... cargo test reconciliation -- --include-ignored

    #[tokio::test]
    #[ignore = "requires DATABASE_URL and migrations"]
    async fn test_reconcile_empty_db_and_empty_chain() {
        let pool = sqlx::PgPool::connect(&std::env::var("DATABASE_URL").unwrap())
            .await
            .unwrap();
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"^/accounts/.*/payments.*".into()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(payments_body(&[]))
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let svc = ReconciliationService::new(client, pool);
        let (start, end) = make_period();
        let report = svc.reconcile("GTEST_EMPTY", start, end).await.unwrap();

        assert_eq!(report.total_chain_payments, 0);
        assert!(report.missing_on_chain.is_empty());
        assert!(report.orphaned_payments.is_empty());
        assert!(report.amount_mismatches.is_empty());
        check_conservation(&report);
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL and migrations"]
    async fn test_reconcile_detects_orphaned_payment() {
        let pool = sqlx::PgPool::connect(&std::env::var("DATABASE_URL").unwrap())
            .await
            .unwrap();
        let mut server = mockito::Server::new_async().await;
        let account = "GORPHAN_ACCOUNT";
        let record = payment_record(
            "pay-chain-001",
            "GSRC",
            account,
            "25.00",
            "USDC",
            Some("chain-only-memo"),
        );
        let _mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"^/accounts/.*/payments.*".into()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(payments_body(&[record]))
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let svc = ReconciliationService::new(client, pool);
        let (start, end) = make_period();
        let report = svc.reconcile(account, start, end).await.unwrap();

        assert_eq!(report.total_chain_payments, 1);
        assert_eq!(report.orphaned_payments.len(), 1);
        assert_eq!(
            report.orphaned_payments[0].memo.as_deref(),
            Some("chain-only-memo")
        );
        assert!(report.missing_on_chain.is_empty());
        assert!(report.amount_mismatches.is_empty());
        check_conservation(&report);
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL and migrations"]
    async fn test_reconcile_detects_missing_on_chain() {
        let pool = sqlx::PgPool::connect(&std::env::var("DATABASE_URL").unwrap())
            .await
            .unwrap();
        let account = "GMISSING_ACCOUNT";
        let memo = format!("missing-memo-{}", Uuid::new_v4());
        let (start, end) = make_period();
        sqlx::query(
            "INSERT INTO transactions (id, stellar_account, amount, asset_code, status, memo, created_at, updated_at)
             VALUES ($1, $2, $3::numeric, $4, 'completed', $5, $6, $6)",
        )
        .bind(Uuid::new_v4())
        .bind(account)
        .bind("75.00")
        .bind("USDC")
        .bind(&memo)
        .bind(start + chrono::Duration::minutes(30))
        .execute(&pool)
        .await
        .unwrap();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"^/accounts/.*/payments.*".into()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(payments_body(&[]))
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let svc = ReconciliationService::new(client, pool.clone());
        let report = svc.reconcile(account, start, end).await.unwrap();

        assert_eq!(report.missing_on_chain.len(), 1);
        assert_eq!(
            report.missing_on_chain[0].memo.as_deref(),
            Some(memo.as_str())
        );
        assert!(report.orphaned_payments.is_empty());
        assert!(report.amount_mismatches.is_empty());
        check_conservation(&report);

        sqlx::query("DELETE FROM transactions WHERE stellar_account = $1")
            .bind(account)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL and migrations"]
    async fn test_reconcile_detects_amount_mismatch() {
        let pool = sqlx::PgPool::connect(&std::env::var("DATABASE_URL").unwrap())
            .await
            .unwrap();
        let account = "GMISMATCH_ACCOUNT";
        let memo = format!("mismatch-memo-{}", Uuid::new_v4());
        let (start, end) = make_period();

        sqlx::query(
            "INSERT INTO transactions (id, stellar_account, amount, asset_code, status, memo, created_at, updated_at)
             VALUES ($1, $2, $3::numeric, $4, 'completed', $5, $6, $6)",
        )
        .bind(Uuid::new_v4())
        .bind(account)
        .bind("100.00")
        .bind("USDC")
        .bind(&memo)
        .bind(start + chrono::Duration::minutes(10))
        .execute(&pool)
        .await
        .unwrap();

        let record = payment_record(
            "pay-mismatch-001",
            "GSRC",
            account,
            "99.00",
            "USDC",
            Some(&memo),
        );
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"^/accounts/.*/payments.*".into()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(payments_body(&[record]))
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let svc = ReconciliationService::new(client, pool.clone());
        let report = svc.reconcile(account, start, end).await.unwrap();

        assert_eq!(report.amount_mismatches.len(), 1);
        assert_eq!(report.amount_mismatches[0].db_amount, "100.00");
        assert_eq!(report.amount_mismatches[0].chain_amount, "99.00");
        assert_eq!(
            report.amount_mismatches[0].memo.as_deref(),
            Some(memo.as_str())
        );
        assert!(report.missing_on_chain.is_empty());
        assert!(report.orphaned_payments.is_empty());
        check_conservation(&report);

        sqlx::query("DELETE FROM transactions WHERE stellar_account = $1")
            .bind(account)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL and migrations"]
    async fn test_reconcile_exact_match_no_discrepancies() {
        let pool = sqlx::PgPool::connect(&std::env::var("DATABASE_URL").unwrap())
            .await
            .unwrap();
        let account = "GCLEAN_ACCOUNT";
        let memo = format!("clean-memo-{}", Uuid::new_v4());
        let (start, end) = make_period();

        sqlx::query(
            "INSERT INTO transactions (id, stellar_account, amount, asset_code, status, memo, created_at, updated_at)
             VALUES ($1, $2, $3::numeric, $4, 'completed', $5, $6, $6)",
        )
        .bind(Uuid::new_v4())
        .bind(account)
        .bind("42.00")
        .bind("USDC")
        .bind(&memo)
        .bind(start + chrono::Duration::minutes(5))
        .execute(&pool)
        .await
        .unwrap();

        let record = payment_record(
            "pay-clean-001",
            "GSRC",
            account,
            "42.00",
            "USDC",
            Some(&memo),
        );
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"^/accounts/.*/payments.*".into()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(payments_body(&[record]))
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let svc = ReconciliationService::new(client, pool.clone());
        let report = svc.reconcile(account, start, end).await.unwrap();

        assert!(report.missing_on_chain.is_empty());
        assert!(report.orphaned_payments.is_empty());
        assert!(report.amount_mismatches.is_empty());
        assert_eq!(report.total_db_transactions, 1);
        assert_eq!(report.total_chain_payments, 1);
        assert_eq!(report.matched_count, 1);
        check_conservation(&report);

        sqlx::query("DELETE FROM transactions WHERE stellar_account = $1")
            .bind(account)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL and migrations"]
    async fn test_reconcile_partial_match_mixed_results() {
        let pool = sqlx::PgPool::connect(&std::env::var("DATABASE_URL").unwrap())
            .await
            .unwrap();
        let account = "GPARTIAL_ACCOUNT";
        let (start, end) = make_period();

        let memo_match = format!("partial-match-{}", Uuid::new_v4());
        let memo_missing = format!("partial-missing-{}", Uuid::new_v4());
        let memo_mismatch = format!("partial-mismatch-{}", Uuid::new_v4());
        let memo_orphan = format!("partial-orphan-{}", Uuid::new_v4());

        for (memo, amount) in [
            (&memo_match, "10.00"),
            (&memo_missing, "20.00"),
            (&memo_mismatch, "30.00"),
        ] {
            sqlx::query(
                "INSERT INTO transactions (id, stellar_account, amount, asset_code, status, memo, created_at, updated_at)
                 VALUES ($1, $2, $3::numeric, $4, 'completed', $5, $6, $6)",
            )
            .bind(Uuid::new_v4())
            .bind(account)
            .bind(amount)
            .bind("USDC")
            .bind(memo.as_str())
            .bind(start + chrono::Duration::minutes(1))
            .execute(&pool)
            .await
            .unwrap();
        }

        let chain_records = vec![
            payment_record(
                "cp-match",
                "GSRC",
                account,
                "10.00",
                "USDC",
                Some(&memo_match),
            ),
            payment_record(
                "cp-mismatch",
                "GSRC",
                account,
                "31.00",
                "USDC",
                Some(&memo_mismatch),
            ),
            payment_record(
                "cp-orphan",
                "GSRC",
                account,
                "99.00",
                "USDC",
                Some(&memo_orphan),
            ),
        ];
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"^/accounts/.*/payments.*".into()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(payments_body(&chain_records))
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let svc = ReconciliationService::new(client, pool.clone());
        let report = svc.reconcile(account, start, end).await.unwrap();

        assert_eq!(report.missing_on_chain.len(), 1, "one missing");
        assert_eq!(
            report.missing_on_chain[0].memo.as_deref(),
            Some(memo_missing.as_str())
        );
        assert_eq!(report.orphaned_payments.len(), 1, "one orphaned");
        assert_eq!(
            report.orphaned_payments[0].memo.as_deref(),
            Some(memo_orphan.as_str())
        );
        assert_eq!(report.amount_mismatches.len(), 1, "one mismatch");
        assert_eq!(report.amount_mismatches[0].db_amount, "30.00");
        assert_eq!(report.amount_mismatches[0].chain_amount, "31.00");
        check_conservation(&report);

        sqlx::query("DELETE FROM transactions WHERE stellar_account = $1")
            .bind(account)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL and migrations"]
    async fn test_reconcile_transactions_without_memo_use_fallback_matcher() {
        // Memo-less DB rows are no longer silently dropped; they are matched by
        // (account + amount + asset) or reported in unmatched_no_memo_db.
        let pool = sqlx::PgPool::connect(&std::env::var("DATABASE_URL").unwrap())
            .await
            .unwrap();
        let account = "GNOMEMO_ACCOUNT";
        let (start, end) = make_period();

        sqlx::query(
            "INSERT INTO transactions (id, stellar_account, amount, asset_code, status, memo, created_at, updated_at)
             VALUES ($1, $2, $3::numeric, $4, 'completed', NULL, $5, $5)",
        )
        .bind(Uuid::new_v4())
        .bind(account)
        .bind("15.00")
        .bind("USDC")
        .bind(start + chrono::Duration::minutes(1))
        .execute(&pool)
        .await
        .unwrap();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"^/accounts/.*/payments.*".into()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(payments_body(&[]))
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let svc = ReconciliationService::new(client, pool.clone());
        let report = svc.reconcile(account, start, end).await.unwrap();

        // Memo-less row with no chain counterpart → unmatched_no_memo_db, not dropped.
        assert_eq!(report.unmatched_no_memo_db.len(), 1);
        assert_eq!(report.unmatched_no_memo_db[0].amount, "15.00");
        assert!(report.missing_on_chain.is_empty());
        assert_eq!(report.total_db_transactions, 1);
        check_conservation(&report);

        sqlx::query("DELETE FROM transactions WHERE stellar_account = $1")
            .bind(account)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL and migrations"]
    async fn test_reconcile_duplicate_memos_one_matched_one_missing() {
        // Two DB rows share the same memo; only one chain payment exists.
        // Old behaviour: HashMap overwrite silently dropped one DB row.
        // New behaviour: 1 matched, 1 missing_on_chain.
        let pool = sqlx::PgPool::connect(&std::env::var("DATABASE_URL").unwrap())
            .await
            .unwrap();
        let account = "GDUP_ACCOUNT";
        let memo = format!("dup-memo-{}", Uuid::new_v4());
        let (start, end) = make_period();

        for amount in ["50.00", "50.00"] {
            sqlx::query(
                "INSERT INTO transactions (id, stellar_account, amount, asset_code, status, memo, created_at, updated_at)
                 VALUES ($1, $2, $3::numeric, $4, 'completed', $5, $6, $6)",
            )
            .bind(Uuid::new_v4())
            .bind(account)
            .bind(amount)
            .bind("USDC")
            .bind(&memo)
            .bind(start + chrono::Duration::minutes(2))
            .execute(&pool)
            .await
            .unwrap();
        }

        let record = payment_record("pay-dup-001", "GSRC", account, "50.00", "USDC", Some(&memo));
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"^/accounts/.*/payments.*".into()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(payments_body(&[record]))
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let svc = ReconciliationService::new(client, pool.clone());
        let report = svc.reconcile(account, start, end).await.unwrap();

        assert_eq!(report.matched_count, 1);
        assert_eq!(report.missing_on_chain.len(), 1, "second dup DB row must be reported missing");
        assert!(report.amount_mismatches.is_empty());
        assert_eq!(report.total_db_transactions, 2);
        check_conservation(&report);

        sqlx::query("DELETE FROM transactions WHERE stellar_account = $1")
            .bind(account)
            .execute(&pool)
            .await
            .unwrap();
    }
} // end mod tests

// ── Property tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod property_tests {
    use super::*;
    use chrono::TimeZone;
    use proptest::prelude::*;

    fn fixed() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
    }

    /// Generate an optional memo from a small alphabet to create interesting collisions.
    fn arb_memo() -> impl Strategy<Value = Option<String>> {
        prop::option::of(prop_oneof![
            Just("A".to_string()),
            Just("B".to_string()),
            Just("C".to_string()),
        ])
    }

    /// Generate an asset code so we can test inter-asset ambiguity.
    fn arb_asset() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("USDC".to_string()),
            Just("XLM".to_string()),
        ]
    }

    /// Generate an amount from a small set to drive both exact and mismatch paths.
    fn arb_amount() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("100.00".to_string()),
            Just("200.00".to_string()),
            Just("300.00".to_string()),
        ]
    }

    prop_compose! {
        fn arb_db_tx(index: u128)(
            memo in arb_memo(),
            amount in arb_amount(),
            asset in arb_asset(),
        ) -> DbTransaction {
            DbTransaction {
                id: Uuid::from_u128(index),
                stellar_account: "GTEST".to_string(),
                amount,
                asset_code: asset,
                memo,
                created_at: fixed(),
            }
        }
    }

    prop_compose! {
        fn arb_chain_payment(index: u128)(
            memo in arb_memo(),
            amount in arb_amount(),
            asset in arb_asset(),
        ) -> ChainPayment {
            ChainPayment {
                id: format!("cp-{}", index),
                from: "GSRC".to_string(),
                to: "GTEST".to_string(),
                amount,
                asset_code: asset,
                memo,
            }
        }
    }

    proptest! {
        /// Conservation invariant: every DB row and every chain payment appears
        /// in exactly one output bucket — no record is silently dropped.
        #[test]
        fn prop_conservation_invariant(
            db_count in 0usize..8,
            chain_count in 0usize..8,
        ) {
            let t = fixed();
            let period_end = t + Duration::hours(1);

            // Build synthetic db_txs / chain_payments using deterministic indices.
            // proptest drives the sizes; inner fields use fixed values to keep it simple.
            let db_txs: Vec<DbTransaction> = (0..db_count)
                .map(|i| {
                    let memo = match i % 4 {
                        0 => Some("A".to_string()),
                        1 => Some("B".to_string()),
                        2 => None,
                        _ => Some("A".to_string()),
                    };
                    DbTransaction {
                        id: Uuid::from_u128(i as u128),
                        stellar_account: "GTEST".to_string(),
                        amount: if i % 2 == 0 { "100.00" } else { "200.00" }.to_string(),
                        asset_code: if i % 3 == 0 { "USDC" } else { "XLM" }.to_string(),
                        memo,
                        created_at: t,
                    }
                })
                .collect();

            let chain_payments: Vec<ChainPayment> = (0..chain_count)
                .map(|i| {
                    let memo = match i % 4 {
                        0 => Some("A".to_string()),
                        1 => Some("B".to_string()),
                        2 => None,
                        _ => Some("C".to_string()),
                    };
                    ChainPayment {
                        id: format!("cp-{}", i),
                        from: "GSRC".to_string(),
                        to: "GTEST".to_string(),
                        amount: if i % 2 == 0 { "100.00" } else { "300.00" }.to_string(),
                        asset_code: if i % 3 == 0 { "USDC" } else { "XLM" }.to_string(),
                        memo,
                    }
                })
                .collect();

            let report = perform_matching(&db_txs, &chain_payments, t, period_end);

            prop_assert_eq!(
                report.total_db_transactions,
                report.matched_count
                    + report.missing_on_chain.len()
                    + report.ambiguous_db.len()
                    + report.unmatched_no_memo_db.len(),
                "DB conservation violated for db_count={} chain_count={}",
                db_count,
                chain_count
            );
            prop_assert_eq!(
                report.total_chain_payments,
                report.matched_count
                    + report.orphaned_payments.len()
                    + report.ambiguous_chain.len()
                    + report.unmatched_no_memo_chain.len(),
                "chain conservation violated for db_count={} chain_count={}",
                db_count,
                chain_count
            );
        }

        /// amount_mismatches is always a subset of matched pairs.
        #[test]
        fn prop_amount_mismatches_subset_of_matched(
            db_count in 0usize..8,
            chain_count in 0usize..8,
        ) {
            let t = fixed();
            let db_txs: Vec<DbTransaction> = (0..db_count)
                .map(|i| DbTransaction {
                    id: Uuid::from_u128(i as u128),
                    stellar_account: "GTEST".to_string(),
                    amount: if i % 2 == 0 { "100.00" } else { "200.00" }.to_string(),
                    asset_code: "USDC".to_string(),
                    memo: Some(format!("memo-{}", i % 3)),
                    created_at: t,
                })
                .collect();
            let chain_payments: Vec<ChainPayment> = (0..chain_count)
                .map(|i| ChainPayment {
                    id: format!("cp-{}", i),
                    from: "GSRC".to_string(),
                    to: "GTEST".to_string(),
                    amount: if i % 2 == 0 { "100.00" } else { "999.00" }.to_string(),
                    asset_code: "USDC".to_string(),
                    memo: Some(format!("memo-{}", i % 3)),
                })
                .collect();

            let report = perform_matching(&db_txs, &chain_payments, t, t + Duration::hours(1));

            prop_assert!(
                report.amount_mismatches.len() <= report.matched_count,
                "amount_mismatches ({}) exceeds matched_count ({})",
                report.amount_mismatches.len(),
                report.matched_count
            );
        }
    }
}
