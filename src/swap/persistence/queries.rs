//! Named access patterns for swap order, quote, and fill lookups (#1526).
//!
//! Every pattern is backed by a composite index created by
//! `ensure_swap_lookup_indexes()` (see
//! `migrations/20261021000000_swap_lookup_indexes.sql`) and documented in
//! `docs/swap-query-plans.md`. Rules each query here must follow:
//!
//! - lead with `tenant_id = $1` (every index starts with it, and RLS scopes by it);
//! - list/range queries bound `created_at` on both sides so partition pruning
//!   engages on the `created_at`-partitioned tables;
//! - order by `created_at DESC, id DESC` to match the index sort order.
//!
//! [`explain_sql`] and [`explain_uses_seq_scan`] back the
//! `swap_lookup_plans_use_indexes` test, which guards against plans regressing
//! to sequential scans.

const TENANT: &str = "'00000000-0000-0000-0000-000000000001'::uuid";
const FROM: &str = "now() - interval '1 day'";
const TO: &str = "now()";

/// A named swap lookup and the index that serves it.
#[derive(Debug, Clone, Copy)]
pub struct AccessPattern {
    pub name: &'static str,
    pub table: &'static str,
    pub index: &'static str,
    pub sql: &'static str,
    /// SQL literals substituted for `$1..$n` when the plan is checked with
    /// `EXPLAIN` (see [`explain_sql`]).
    pub sample_params: &'static [&'static str],
}

pub const ORDERS_BY_STATUS: AccessPattern = AccessPattern {
    name: "orders_by_tenant_status",
    table: "swap_orders",
    index: "idx_swap_orders_tenant_status_created",
    sql: "SELECT * FROM swap_orders \
          WHERE tenant_id = $1 AND status = $2 AND created_at >= $3 AND created_at < $4 \
          ORDER BY created_at DESC, id DESC LIMIT $5",
    sample_params: &[TENANT, "'pending'", FROM, TO, "50"],
};

pub const ORDERS_BY_TIME_RANGE: AccessPattern = AccessPattern {
    name: "orders_by_tenant_time_range",
    table: "swap_orders",
    index: "idx_swap_orders_tenant_created",
    sql: "SELECT * FROM swap_orders \
          WHERE tenant_id = $1 AND created_at >= $2 AND created_at < $3 \
          ORDER BY created_at DESC, id DESC LIMIT $4",
    sample_params: &[TENANT, FROM, TO, "50"],
};

pub const ORDERS_BY_ASSET_PAIR: AccessPattern = AccessPattern {
    name: "orders_by_tenant_asset_pair",
    table: "swap_orders",
    index: "idx_swap_orders_tenant_pair_created",
    sql: "SELECT * FROM swap_orders \
          WHERE tenant_id = $1 AND source_asset = $2 AND target_asset = $3 \
          AND created_at >= $4 AND created_at < $5 \
          ORDER BY created_at DESC, id DESC LIMIT $6",
    sample_params: &[TENANT, "'USDC'", "'XLM'", FROM, TO, "50"],
};

pub const ORDER_BY_IDEMPOTENCY_KEY: AccessPattern = AccessPattern {
    name: "order_by_idempotency_key",
    table: "swap_orders",
    index: "idx_swap_orders_tenant_idempotency",
    sql: "SELECT * FROM swap_orders WHERE tenant_id = $1 AND idempotency_key = $2",
    sample_params: &[TENANT, "'idem-1'"],
};

pub const ORDER_BY_EXTERNAL_REF: AccessPattern = AccessPattern {
    name: "order_by_external_ref",
    table: "swap_orders",
    index: "idx_swap_orders_tenant_external_ref",
    sql: "SELECT * FROM swap_orders WHERE tenant_id = $1 AND external_ref = $2",
    sample_params: &[TENANT, "'ext-1'"],
};

pub const QUOTES_BY_ASSET_PAIR: AccessPattern = AccessPattern {
    name: "quotes_by_tenant_asset_pair",
    table: "swap_quotes",
    index: "idx_swap_quotes_tenant_pair_created",
    sql: "SELECT * FROM swap_quotes \
          WHERE tenant_id = $1 AND source_asset = $2 AND target_asset = $3 \
          AND created_at >= $4 AND created_at < $5 \
          ORDER BY created_at DESC, id DESC LIMIT $6",
    sample_params: &[TENANT, "'USDC'", "'XLM'", FROM, TO, "50"],
};

pub const QUOTES_BY_STATUS: AccessPattern = AccessPattern {
    name: "quotes_by_tenant_status",
    table: "swap_quotes",
    index: "idx_swap_quotes_tenant_status_created",
    sql: "SELECT * FROM swap_quotes \
          WHERE tenant_id = $1 AND status = $2 AND created_at >= $3 AND created_at < $4 \
          ORDER BY created_at DESC, id DESC LIMIT $5",
    sample_params: &[TENANT, "'open'", FROM, TO, "50"],
};

pub const FILLS_BY_ORDER: AccessPattern = AccessPattern {
    name: "fills_by_order",
    table: "swap_fills",
    index: "idx_swap_fills_tenant_order_created",
    sql: "SELECT * FROM swap_fills \
          WHERE tenant_id = $1 AND order_id = $2 AND created_at >= $3 AND created_at < $4 \
          ORDER BY created_at DESC, id DESC",
    sample_params: &[TENANT, TENANT, FROM, TO],
};

pub const FILLS_BY_TIME_RANGE: AccessPattern = AccessPattern {
    name: "fills_by_tenant_time_range",
    table: "swap_fills",
    index: "idx_swap_fills_tenant_created",
    sql: "SELECT * FROM swap_fills \
          WHERE tenant_id = $1 AND created_at >= $2 AND created_at < $3 \
          ORDER BY created_at DESC, id DESC LIMIT $4",
    sample_params: &[TENANT, FROM, TO, "50"],
};

pub const FILL_BY_EXTERNAL_REF: AccessPattern = AccessPattern {
    name: "fill_by_external_ref",
    table: "swap_fills",
    index: "idx_swap_fills_tenant_external_ref",
    sql: "SELECT * FROM swap_fills WHERE tenant_id = $1 AND external_ref = $2",
    sample_params: &[TENANT, "'ext-1'"],
};

/// All access patterns, in the order they appear in `docs/swap-query-plans.md`.
pub const ACCESS_PATTERNS: &[AccessPattern] = &[
    ORDERS_BY_STATUS,
    ORDERS_BY_TIME_RANGE,
    ORDERS_BY_ASSET_PAIR,
    ORDER_BY_IDEMPOTENCY_KEY,
    ORDER_BY_EXTERNAL_REF,
    QUOTES_BY_ASSET_PAIR,
    QUOTES_BY_STATUS,
    FILLS_BY_ORDER,
    FILLS_BY_TIME_RANGE,
    FILL_BY_EXTERNAL_REF,
];

/// `EXPLAIN` statement for a pattern with its sample parameters inlined.
/// Placeholders are replaced highest-first so `$1` never clobbers `$10`.
pub fn explain_sql(p: &AccessPattern) -> String {
    let mut sql = p.sql.to_string();
    for (i, v) in p.sample_params.iter().enumerate().rev() {
        sql = sql.replace(&format!("${}", i + 1), v);
    }
    format!("EXPLAIN {sql}")
}

/// `true` when an `EXPLAIN` text plan scans the swap table sequentially.
pub fn explain_uses_seq_scan(plan: &str, table: &str) -> bool {
    plan.lines().any(|l| {
        let l = l.trim_start_matches([' ', '-', '>']);
        l.starts_with("Seq Scan on ") && l["Seq Scan on ".len()..].starts_with(table)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIGRATION: &str =
        include_str!("../../../migrations/20261021000000_swap_lookup_indexes.sql");

    #[test]
    fn every_pattern_is_tenant_scoped() {
        for p in ACCESS_PATTERNS {
            assert!(p.sql.contains("WHERE tenant_id = $1"), "{}", p.name);
        }
    }

    #[test]
    fn range_patterns_bound_created_at_for_partition_pruning() {
        for p in ACCESS_PATTERNS
            .iter()
            .filter(|p| p.sql.contains("ORDER BY"))
        {
            assert!(
                p.sql.contains("created_at >=") && p.sql.contains("created_at <"),
                "{} must bound created_at on both sides",
                p.name
            );
        }
    }

    #[test]
    fn every_pattern_index_is_created_by_the_migration() {
        for p in ACCESS_PATTERNS {
            assert!(
                MIGRATION.contains(p.index),
                "{} missing {}",
                p.name,
                p.index
            );
        }
    }

    #[test]
    fn sample_params_cover_every_placeholder() {
        for p in ACCESS_PATTERNS {
            let sql = explain_sql(p);
            assert!(
                !sql.contains('$'),
                "{}: unbound placeholder in {sql}",
                p.name
            );
        }
    }

    /// Plan regression guard. With sequential scans disabled, a plan that
    /// still contains `Seq Scan` on the swap table means no index can serve
    /// the pattern. Requires `DATABASE_URL` pointing at a migrated database;
    /// patterns whose table does not exist yet are skipped.
    /// Run: `cargo test --lib swap_lookup_plans_use_indexes -- --ignored`
    #[tokio::test]
    #[ignore]
    async fn swap_lookup_plans_use_indexes() {
        use sqlx::{Executor, Row};
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
        let pool = sqlx::PgPool::connect(&url).await.unwrap();
        let mut conn = pool.acquire().await.unwrap();
        conn.execute("SET enable_seqscan = off").await.unwrap();
        for p in ACCESS_PATTERNS {
            let exists: Option<String> = sqlx::query_scalar("SELECT to_regclass($1)::text")
                .bind(p.table)
                .fetch_one(&mut *conn)
                .await
                .unwrap();
            if exists.is_none() {
                continue;
            }
            let rows = conn.fetch_all(explain_sql(p).as_str()).await.unwrap();
            let plan: Vec<String> = rows.iter().map(|r| r.get::<String, _>(0)).collect();
            let plan = plan.join("\n");
            assert!(
                !explain_uses_seq_scan(&plan, p.table),
                "{}:\n{plan}",
                p.name
            );
        }
    }

    #[test]
    fn detects_seq_scan_in_plan() {
        let bad = "Limit\n  ->  Seq Scan on swap_orders_y2026m10 swap_orders\n";
        let good = "Limit\n  ->  Index Scan using idx_swap_orders_tenant_created on swap_orders_y2026m10\n";
        assert!(explain_uses_seq_scan(bad, "swap_orders"));
        assert!(!explain_uses_seq_scan(good, "swap_orders"));
        assert!(!explain_uses_seq_scan(bad, "swap_fills"));
    }
}
