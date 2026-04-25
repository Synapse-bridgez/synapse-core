use criterion::{black_box, criterion_group, criterion_main, Criterion};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use sqlx::types::BigDecimal;

// ---------------------------------------------------------------------------
// Benchmark: callback payload JSON schema validation
// ---------------------------------------------------------------------------

fn bench_callback_validation(c: &mut Criterion) {
    use synapse_core::validation::schemas::SCHEMAS;

    let valid = serde_json::json!({
        "stellar_account": "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "amount": "100.50",
        "asset_code": "USD",
        "callback_type": "deposit",
        "callback_status": "completed",
        "anchor_transaction_id": "anchor-bench-001",
        "memo": "bench memo",
        "memo_type": "text"
    });

    // Force schema initialisation outside the timed loop.
    let _ = SCHEMAS.callback_v1.validate(&valid);

    c.bench_function("callback_payload_validation", |b| {
        b.iter(|| {
            let _ = SCHEMAS.callback_v1.validate(black_box(&valid));
        })
    });
}

// ---------------------------------------------------------------------------
// Benchmark: Transaction::new (in-memory construction)
// ---------------------------------------------------------------------------

fn bench_transaction_construction(c: &mut Criterion) {
    use synapse_core::db::models::Transaction;

    let amount: BigDecimal = "100.50".parse().unwrap();

    c.bench_function("transaction_construction", |b| {
        b.iter(|| {
            Transaction::new(
                black_box("GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string()),
                black_box(amount.clone()),
                black_box("USD".to_string()),
                black_box(Some("anchor-001".to_string())),
                black_box(Some("deposit".to_string())),
                black_box(Some("completed".to_string())),
                black_box(Some("bench memo".to_string())),
                black_box(Some("text".to_string())),
                black_box(None),
            )
        })
    });
}

// ---------------------------------------------------------------------------
// Benchmark: search query WHERE clause construction (mirrors queries.rs logic)
// ---------------------------------------------------------------------------

fn bench_search_query_construction(c: &mut Criterion) {
    c.bench_function("search_query_construction", |b| {
        b.iter(|| {
            let status = black_box(Some("completed"));
            let asset_code = black_box(Some("USD"));
            let min_amount: Option<BigDecimal> = black_box(Some("10".parse().unwrap()));
            let max_amount: Option<BigDecimal> = black_box(Some("1000".parse().unwrap()));

            let mut conditions = Vec::new();
            let mut param_count = 1usize;

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

            let where_clause = if conditions.is_empty() {
                String::new()
            } else {
                format!("WHERE {}", conditions.join(" AND "))
            };

            let _ = format!(
                "SELECT * FROM transactions {} ORDER BY created_at DESC LIMIT ${}",
                where_clause, param_count
            );
        })
    });
}

// ---------------------------------------------------------------------------
// Benchmark: HMAC-SHA256 signing (webhook signature verification path)
// ---------------------------------------------------------------------------

fn bench_hmac_signing(c: &mut Criterion) {
    type HmacSha256 = Hmac<Sha256>;

    let secret = b"super-secret-webhook-key-for-bench";
    let payload = b"{\"id\":\"evt_bench_001\",\"type\":\"transaction.completed\",\"data\":{\"amount\":\"100.50\",\"asset_code\":\"USD\"}}";

    c.bench_function("hmac_sha256_signing", |b| {
        b.iter(|| {
            let mut mac = HmacSha256::new_from_slice(black_box(secret)).unwrap();
            mac.update(black_box(payload));
            let _ = mac.finalize().into_bytes();
        })
    });
}

// ---------------------------------------------------------------------------
// Benchmark: cursor encode/decode round-trip
// ---------------------------------------------------------------------------

fn bench_cursor_roundtrip(c: &mut Criterion) {
    use chrono::Utc;
    use synapse_core::utils::cursor;
    use uuid::Uuid;

    let ts = Utc::now();
    let id = Uuid::new_v4();
    let encoded = cursor::encode(ts, id);

    c.bench_function("cursor_encode", |b| {
        b.iter(|| cursor::encode(black_box(ts), black_box(id)))
    });

    c.bench_function("cursor_decode", |b| {
        b.iter(|| cursor::decode(black_box(&encoded)).unwrap())
    });
}

criterion_group!(
    benches,
    bench_callback_validation,
    bench_transaction_construction,
    bench_search_query_construction,
    bench_hmac_signing,
    bench_cursor_roundtrip,
);
criterion_main!(benches);
