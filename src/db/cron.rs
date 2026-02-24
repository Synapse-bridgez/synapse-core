use chrono::{Datelike, NaiveDate, TimeZone, Utc};
use sqlx::postgres::PgPool;
use sqlx::Row;

pub async fn create_month_partition(pool: &PgPool, year: i32, month: u32) -> Result<(), sqlx::Error> {
    let month = if month == 0 { 1 } else { month };
    let start = NaiveDate::from_ymd_opt(year, month, 1).unwrap().and_hms_opt(0, 0, 0).unwrap();
    // compute next month
    let (ny, nm) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    let end = NaiveDate::from_ymd_opt(ny, nm, 1).unwrap().and_hms_opt(0, 0, 0).unwrap();

    let part_name = format!("transactions_y{}m{:02}", year, month);
    let start_ts = chrono::DateTime::<Utc>::from_utc(start, Utc).to_rfc3339();
    let end_ts = chrono::DateTime::<Utc>::from_utc(end, Utc).to_rfc3339();

    let create_sql = format!(
        "CREATE TABLE IF NOT EXISTS \"{}\" PARTITION OF transactions FOR VALUES FROM (TIMESTAMP WITH TIME ZONE '{}') TO (TIMESTAMP WITH TIME ZONE '{}')",
        part_name, start_ts, end_ts
    );

    sqlx::query(&create_sql).execute(pool).await?;
    let idx1 = format!("CREATE INDEX IF NOT EXISTS idx_{}_status ON \"{}\" (status)", part_name, part_name);
    let idx2 = format!("CREATE INDEX IF NOT EXISTS idx_{}_stellar_account ON \"{}\" (stellar_account)", part_name, part_name);
    sqlx::query(&idx1).execute(pool).await?;
    sqlx::query(&idx2).execute(pool).await?;

    Ok(())
}

/// Detach partitions older than `retention_months` and move them to `archive` schema.
pub async fn detach_and_archive_old_partitions(pool: &PgPool, retention_months: i64) -> Result<(), sqlx::Error> {
    // compute cutoff year-month
    let now = Utc::now();
    let cutoff = now - chrono::Duration::days(30 * retention_months);

    // fetch child partitions of `transactions`
    let rows = sqlx::query("SELECT c.relname as child FROM pg_inherits i JOIN pg_class c ON i.inhrelid = c.oid JOIN pg_class p ON i.inhparent = p.oid WHERE p.relname = 'transactions'")
        .fetch_all(pool)
        .await?;

    // ensure archive schema exists
    sqlx::query("CREATE SCHEMA IF NOT EXISTS archive").execute(pool).await?;

    for row in rows {
        let child: String = row.get("child");
        // expect names like transactions_y2025m02
        if let Some((y, m)) = parse_partition_name(&child) {
            let part_date = Utc.ymd_opt(y, m, 1).single().unwrap().and_hms_opt(0, 0, 0).unwrap();
            if part_date < cutoff {
                // detach
                let detach_sql = format!("ALTER TABLE transactions DETACH PARTITION \"{}\"", child);
                sqlx::query(&detach_sql).execute(pool).await?;
                // move to archive schema
                let set_schema = format!("ALTER TABLE \"{}\" SET SCHEMA archive", child);
                sqlx::query(&set_schema).execute(pool).await?;
            }
        }
    }

    Ok(())
}

fn parse_partition_name(name: &str) -> Option<(i32, u32)> {
    // Very small parser for expected pattern transactions_yYYYYmMM
    if !name.starts_with("transactions_y") {
        return None;
    }
    let rest = &name[14..]; // after transactions_y
    if let Some(idx) = rest.find('m') {
        let y = &rest[..idx];
        let m = &rest[idx+1..];
        if let (Ok(yy), Ok(mm)) = (y.parse::<i32>(), m.parse::<u32>()) {
            return Some((yy, mm));
        }
    }
    None
}

/// Convenience: create partitions for the next `months_ahead` months (including current month).
pub async fn ensure_future_partitions(pool: &PgPool, months_ahead: u32) -> Result<(), sqlx::Error> {
    let now = Utc::now();
    let mut y = now.year();
    let mut m = now.month();
    for _ in 0..months_ahead {
        let _ = create_month_partition(pool, y, m).await?;
        // increment month
        if m == 12 {
            m = 1;
            y += 1;
        } else {
            m += 1;
        }
    }
    Ok(())
}
