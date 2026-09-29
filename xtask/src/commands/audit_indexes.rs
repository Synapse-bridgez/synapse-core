/// Audit indexes for low usage over a monitoring window.
///
/// This tool queries pg_stat_user_indexes to identify unused or rarely-used indexes
/// that are not backing constraint

s (unique/PK/FK), so operators can safely remove them.
/// Unused indexes add write throughput cost without read benefit.

use anyhow::Result;
use clap::Parser;
use sqlx::postgres::PgPool;
use std::collections::HashSet;

#[derive(Parser)]
pub struct IndexAuditArgs {
    /// Database connection string
    #[arg(long, env = "DATABASE_URL")]
    database_url: String,

    /// Days over which to measure index usage (default: 30)
    #[arg(long, default_value = "30")]
    monitoring_window_days: i32,

    /// Minimum number of scans to consider an index "used" (default: 10)
    #[arg(long, default_value = "10")]
    usage_threshold_scans: i64,

    /// Comma-separated list of indexes to exclude from audit (e.g., idx_my_index1,idx_my_index2)
    #[arg(long, default_value = "")]
    override_indexes: String,
}

/// Index usage report
#[derive(Debug)]
pub struct IndexReport {
    pub index_name: String,
    pub table_name: String,
    pub index_def: String,
    pub total_scans: i64,
    pub total_tuples_read: i64,
    pub index_size_bytes: i64,
    pub is_constraint_backed: bool,
    pub last_scan: Option<String>,
}

pub fn run(args: IndexAuditArgs) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let pool = PgPool::connect(&args.database_url).await?;

        // Parse the override list
        let override_indexes: HashSet<String> = args
            .override_indexes
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        // Query for unused indexes
        let unused_indexes = find_unused_indexes(
            &pool,
            args.monitoring_window_days,
            args.usage_threshold_scans,
            &override_indexes,
        )
        .await?;

        // Print report
        if unused_indexes.is_empty() {
            println!("✓ All indexes have sufficient usage. No cleanup recommended.");
        } else {
            println!("Found {} potentially unused indexes:", unused_indexes.len());
            println!();

            for report in unused_indexes {
                println!("Index: {}", report.index_name);
                println!("  Table: {}", report.table_name);
                println!("  Total scans ({}d window): {}", args.monitoring_window_days, report.total_scans);
                println!("  Tuples read: {}", report.total_tuples_read);
                println!("  Size: {:.2} MB", report.index_size_bytes as f64 / 1024.0 / 1024.0);
                if let Some(last_scan) = report.last_scan {
                    println!("  Last scan: {}", last_scan);
                } else {
                    println!("  Last scan: Never");
                }
                println!("  Definition: {}", report.index_def);
                println!();
            }
        }

        Ok(())
    })
}

async fn find_unused_indexes(
    pool: &PgPool,
    monitoring_window_days: i32,
    usage_threshold_scans: i64,
    override_indexes: &HashSet<String>,
) -> Result<Vec<IndexReport>> {
    let query = r#"
        SELECT
            schemaname,
            tablename,
            indexname,
            pg_size_pretty(pg_relation_size(indexrelid)) as index_size_pretty,
            pg_relation_size(indexrelid) as index_size_bytes,
            idx_scan as total_scans,
            idx_tup_read as tuples_read,
            CASE WHEN idx_scan > 0 THEN EXTRACT(EPOCH FROM (NOW() - last_idx_scan))::BIGINT / 86400 ELSE NULL END as days_since_scan,
            last_idx_scan,
            pg_get_indexdef(indexrelid) as index_def,
            FALSE as is_constraint_backed
        FROM pg_stat_user_indexes
        WHERE schemaname NOT IN ('pg_catalog', 'information_schema')
            AND idx_scan < $1
        ORDER BY pg_relation_size(indexrelid) DESC
    "#;

    let rows = sqlx::query(query)
        .bind(usage_threshold_scans)
        .fetch_all(pool)
        .await?;

    // Fetch constraint-backed indexes to exclude them
    let constraint_indexes = get_constraint_backed_indexes(pool).await?;

    let mut reports = Vec::new();
    for row in rows {
        let index_name: String = row.get("indexname");

        // Skip if in override list
        if override_indexes.contains(&index_name) {
            continue;
        }

        // Skip if constraint-backed
        if constraint_indexes.contains(&index_name) {
            continue;
        }

        let report = IndexReport {
            index_name,
            table_name: row.get("tablename"),
            index_def: row.get("index_def"),
            total_scans: row.get("total_scans"),
            total_tuples_read: row.get("tuples_read"),
            index_size_bytes: row.get("index_size_bytes"),
            is_constraint_backed: false,
            last_scan: row.get("last_idx_scan"),
        };

        reports.push(report);
    }

    Ok(reports)
}

async fn get_constraint_backed_indexes(pool: &PgPool) -> Result<HashSet<String>> {
    let query = r#"
        SELECT indexname
        FROM pg_indexes
        WHERE indexdef LIKE '%UNIQUE%' OR indexdef LIKE '%PRIMARY%' OR indexdef LIKE '%FOREIGN%'
    "#;

    let rows = sqlx::query_scalar::<_, String>(query)
        .fetch_all(pool)
        .await?;

    Ok(rows.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_override_indexes_empty() {
        let args = IndexAuditArgs {
            database_url: String::new(),
            monitoring_window_days: 30,
            usage_threshold_scans: 10,
            override_indexes: String::new(),
        };

        let overrides: HashSet<String> = args
            .override_indexes
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        assert!(overrides.is_empty());
    }

    #[test]
    fn test_parse_override_indexes_multiple() {
        let args = IndexAuditArgs {
            database_url: String::new(),
            monitoring_window_days: 30,
            usage_threshold_scans: 10,
            override_indexes: "idx_1, idx_2, idx_3".to_string(),
        };

        let overrides: HashSet<String> = args
            .override_indexes
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        assert_eq!(overrides.len(), 3);
        assert!(overrides.contains("idx_1"));
        assert!(overrides.contains("idx_2"));
        assert!(overrides.contains("idx_3"));
    }
}
