//! Domain-level mapping for the Phase 1 → Phase 2 fiat backfill (#1475).
//!
//! Translates a [`BackfillSourceRow`] (a completed Phase 1 transaction) into
//! the [`SwapQuote`] and [`SwapOrder`] rows that represent it in the Phase 2
//! swap domain.  This module contains only the mapping logic; I/O lives in
//! the runner (`scripts/` or `xtask`).
//!
//! # Idempotency
//! The idempotency key `"swap:{transaction_id}"` is the same key that
//! [`SwapHandoff::from_transaction`] uses for live traffic.  The database
//! `ON CONFLICT DO NOTHING` in `swap_backfill_state` is the first guard; any
//! `INSERT` into `swap_quotes` / `swap_orders` that arrives after a live
//! hand-off or a previous backfill pass will find the same conflict key and
//! do nothing.

use crate::db::backfill_queries::BackfillSourceRow;
use crate::db::models::{SwapOrder, SwapQuote};
use chrono::{Duration, Utc};
use sqlx::types::BigDecimal;
use uuid::Uuid;

/// A unit-rate quote synthesised for a historical fiat deposit.  The rate is
/// set to 1:1 because Phase 1 transactions did not have a swap rate — the
/// backfill records them as already-executed fills at par.
const BACKFILL_SYNTHETIC_RATE: &str = "1.0";

/// Fee amount recorded for backfilled orders — zero because the Phase 1 path
/// did not have a swap fee.
const BACKFILL_FEE_AMOUNT: &str = "0.0";

/// How long the synthetic quote is considered "valid" when written.
/// Backfilled quotes are already expired by design (the swap already happened);
/// this TTL is just the DB value that makes the schema constraint pass.
const BACKFILL_QUOTE_TTL_SECS: i64 = 60;

/// Destination asset used for backfilled orders that have no explicit target.
/// Phase 1 transactions were fiat → Stellar, so XLM is a reasonable canonical
/// destination; operators can override this via the `--dest-asset` CLI flag.
pub const DEFAULT_DEST_ASSET: &str = "XLM";

/// All data produced from a single Phase 1 transaction.
#[derive(Debug)]
pub struct BackfillRecord {
    pub quote: SwapQuote,
    pub order: SwapOrder,
}

/// Map a completed Phase 1 transaction into a synthetic quote + order pair.
///
/// Both rows share the same `created_at` as the source transaction so
/// partition routing lands them in the correct monthly partition.  The
/// idempotency key on the order is `"swap:{transaction_id}"` — identical to
/// the live hand-off path.
pub fn map_transaction_to_backfill_record(
    row: &BackfillSourceRow,
    dest_asset: &str,
) -> BackfillRecord {
    let now = row.created_at; // anchor to the source row's timestamp
    let rate = BACKFILL_SYNTHETIC_RATE
        .parse::<BigDecimal>()
        .expect("BACKFILL_SYNTHETIC_RATE is a valid decimal");
    let fee = BACKFILL_FEE_AMOUNT
        .parse::<BigDecimal>()
        .expect("BACKFILL_FEE_AMOUNT is a valid decimal");

    // Synthetic destination amount = source amount * rate = source amount (1:1)
    let dest_amount = row.amount.clone();

    let quote = SwapQuote {
        id: Uuid::new_v4(),
        tenant_id: None,
        stellar_account: row.stellar_account.clone(),
        source_asset: row.asset_code.clone(),
        destination_asset: dest_asset.to_owned(),
        source_amount: row.amount.clone(),
        destination_amount: dest_amount.clone(),
        rate: rate.clone(),
        fee_amount: fee.clone(),
        expires_at: now + Duration::seconds(BACKFILL_QUOTE_TTL_SECS),
        created_at: now,
        metadata: build_quote_metadata(row),
    };

    let order = SwapOrder {
        id: Uuid::new_v4(),
        tenant_id: None,
        stellar_account: row.stellar_account.clone(),
        quote_id: quote.id,
        quote_created_at: quote.created_at,
        source_asset: row.asset_code.clone(),
        destination_asset: dest_asset.to_owned(),
        source_amount: row.amount.clone(),
        // Backfilled orders are fully filled: filled_amount == source_amount
        filled_amount: row.amount.clone(),
        // "filled" is the terminal success status in the Phase 2 domain
        status: "filled".to_string(),
        created_at: now,
        updated_at: row.updated_at,
        metadata: build_order_metadata(row),
    };

    BackfillRecord { quote, order }
}

fn build_quote_metadata(row: &BackfillSourceRow) -> Option<serde_json::Value> {
    Some(serde_json::json!({
        "backfill": true,
        "source": "phase1_fiat_transaction",
        "transaction_id": row.id.to_string(),
        "anchor_transaction_id": row.anchor_transaction_id,
    }))
}

fn build_order_metadata(row: &BackfillSourceRow) -> Option<serde_json::Value> {
    let mut meta = serde_json::json!({
        "backfill": true,
        "source": "phase1_fiat_transaction",
        "transaction_id": row.id.to_string(),
        "anchor_transaction_id": row.anchor_transaction_id,
        "callback_status": row.callback_status,
        "idempotency_key": format!("swap:{}", row.id),
    });

    // Carry through the original Phase 1 metadata if present
    if let Some(original) = &row.metadata {
        meta["original_metadata"] = original.clone();
    }

    Some(meta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn sample_row() -> BackfillSourceRow {
        BackfillSourceRow {
            id: Uuid::new_v4(),
            stellar_account: "GABCD1234567890ABCD1234567890ABCD1234567890ABCD1234567890".to_string(),
            amount: "250.00".parse::<BigDecimal>().unwrap(),
            asset_code: "USDC".to_string(),
            callback_status: Some("completed".to_string()),
            anchor_transaction_id: Some("anchor-tx-42".to_string()),
            memo: None,
            memo_type: None,
            metadata: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn maps_source_fields_correctly() {
        let row = sample_row();
        let record = map_transaction_to_backfill_record(&row, DEFAULT_DEST_ASSET);

        assert_eq!(record.quote.stellar_account, row.stellar_account);
        assert_eq!(record.quote.source_asset, "USDC");
        assert_eq!(record.quote.destination_asset, "XLM");
        assert_eq!(record.quote.source_amount, row.amount);
        // 1:1 rate → destination_amount == source_amount
        assert_eq!(record.quote.destination_amount, row.amount);

        assert_eq!(record.order.source_asset, "USDC");
        assert_eq!(record.order.destination_asset, "XLM");
        assert_eq!(record.order.source_amount, row.amount);
        assert_eq!(record.order.filled_amount, row.amount);
        assert_eq!(record.order.status, "filled");
    }

    #[test]
    fn order_references_its_own_quote() {
        let row = sample_row();
        let record = map_transaction_to_backfill_record(&row, DEFAULT_DEST_ASSET);
        assert_eq!(record.order.quote_id, record.quote.id);
        assert_eq!(record.order.quote_created_at, record.quote.created_at);
    }

    #[test]
    fn timestamps_anchored_to_source_row() {
        let row = sample_row();
        let record = map_transaction_to_backfill_record(&row, DEFAULT_DEST_ASSET);
        assert_eq!(record.quote.created_at, row.created_at);
        assert_eq!(record.order.created_at, row.created_at);
        assert_eq!(record.order.updated_at, row.updated_at);
    }

    #[test]
    fn idempotency_key_embedded_in_order_metadata() {
        let row = sample_row();
        let record = map_transaction_to_backfill_record(&row, DEFAULT_DEST_ASSET);
        let meta = record.order.metadata.as_ref().unwrap();
        let key = meta["idempotency_key"].as_str().unwrap();
        assert_eq!(key, format!("swap:{}", row.id));
    }

    #[test]
    fn custom_dest_asset_propagates() {
        let row = sample_row();
        let record = map_transaction_to_backfill_record(&row, "USDT");
        assert_eq!(record.quote.destination_asset, "USDT");
        assert_eq!(record.order.destination_asset, "USDT");
    }

    #[test]
    fn backfill_flag_set_in_metadata() {
        let row = sample_row();
        let record = map_transaction_to_backfill_record(&row, DEFAULT_DEST_ASSET);
        let order_meta = record.order.metadata.as_ref().unwrap();
        assert_eq!(order_meta["backfill"], serde_json::json!(true));
        let quote_meta = record.quote.metadata.as_ref().unwrap();
        assert_eq!(quote_meta["backfill"], serde_json::json!(true));
    }
}
