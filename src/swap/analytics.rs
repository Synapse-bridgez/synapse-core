//! Swap analytics rollups (#1506).
//!
//! Hourly/daily/monthly buckets in **UTC**, half-open `[start, end)`, derived
//! from `swap_activity`. A bucket is recomputed wholesale (delete + insert in
//! one transaction), so rollups are idempotent and a bug is fixed by re-running
//! the window. Late events are handled by always recomputing the trailing
//! `lookback` buckets on each tick.

use chrono::{DateTime, Datelike, Duration, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Granularity {
    Hour,
    Day,
    Month,
}

impl Granularity {
    pub const ALL: [Granularity; 3] = [Granularity::Hour, Granularity::Day, Granularity::Month];

    pub fn as_str(&self) -> &'static str {
        match self {
            Granularity::Hour => "hour",
            Granularity::Day => "day",
            Granularity::Month => "month",
        }
    }

    /// Hard cap on the queryable range for this granularity.
    pub fn max_range(&self) -> Duration {
        match self {
            Granularity::Hour => Duration::days(31),
            Granularity::Day => Duration::days(366),
            Granularity::Month => Duration::days(366 * 5),
        }
    }

    /// Start of the bucket containing `t`.
    pub fn bucket_start(&self, t: DateTime<Utc>) -> DateTime<Utc> {
        let (y, m, d) = (t.year(), t.month(), t.day());
        match self {
            Granularity::Hour => Utc.with_ymd_and_hms(y, m, d, t.hour(), 0, 0).unwrap(),
            Granularity::Day => Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap(),
            Granularity::Month => Utc.with_ymd_and_hms(y, m, 1, 0, 0, 0).unwrap(),
        }
    }

    /// Exclusive end of the bucket starting at `start`.
    pub fn bucket_end(&self, start: DateTime<Utc>) -> DateTime<Utc> {
        match self {
            Granularity::Hour => start + Duration::hours(1),
            Granularity::Day => start + Duration::days(1),
            Granularity::Month => {
                let (y, m) = if start.month() == 12 {
                    (start.year() + 1, 1)
                } else {
                    (start.year(), start.month() + 1)
                };
                Utc.with_ymd_and_hms(y, m, 1, 0, 0, 0).unwrap()
            }
        }
    }

    /// A bucket is complete once `now` is at or past its end; the current
    /// bucket may still receive (or be corrected by late) events.
    pub fn is_complete(&self, bucket_start: DateTime<Utc>, now: DateTime<Utc>) -> bool {
        now >= self.bucket_end(bucket_start)
    }
}

/// Recomputes one bucket from `swap_activity` (idempotent).
pub async fn recompute_bucket(
    pool: &PgPool,
    g: Granularity,
    start: DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    let end = g.bucket_end(start);
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM swap_analytics_buckets WHERE granularity = $1 AND bucket_start = $2")
        .bind(g.as_str())
        .bind(start)
        .execute(&mut *tx)
        .await?;
    let n = sqlx::query(
        "INSERT INTO swap_analytics_buckets
           (tenant_id, granularity, bucket_start, pair, direction, status,
            order_count, volume, avg_slippage_bps, median_settlement_ms)
         SELECT tenant_id, $1, $2, pair, direction, status,
                COUNT(*), COALESCE(SUM(amount), 0), AVG(slippage_bps),
                percentile_cont(0.5) WITHIN GROUP (ORDER BY settlement_ms)
         FROM swap_activity
         WHERE kind = 'order' AND created_at >= $2 AND created_at < $3
         GROUP BY tenant_id, pair, direction, status",
    )
    .bind(g.as_str())
    .bind(start)
    .bind(end)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(n)
}

/// Recomputes the current bucket plus `lookback` closed ones for every
/// granularity, which absorbs late-arriving events.
pub async fn rollup_recent(
    pool: &PgPool,
    now: DateTime<Utc>,
    lookback: u32,
) -> Result<(), sqlx::Error> {
    for g in Granularity::ALL {
        let mut start = g.bucket_start(now);
        for _ in 0..=lookback {
            recompute_bucket(pool, g, start).await?;
            start = g.bucket_start(start - Duration::seconds(1));
        }
    }
    Ok(())
}

/// Background rollup worker; opt-in via `SWAP_ANALYTICS_ENABLED=true`.
pub fn spawn(pool: PgPool) {
    if std::env::var("SWAP_ANALYTICS_ENABLED").ok().as_deref() != Some("true") {
        return;
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            tick.tick().await;
            if let Err(e) = rollup_recent(&pool, Utc::now(), 2).await {
                tracing::error!(error = %e, "swap analytics rollup failed");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, mi, 0).unwrap()
    }

    #[test]
    fn bucket_starts_truncate_in_utc() {
        let t = ts(2026, 3, 15, 13, 45);
        assert_eq!(Granularity::Hour.bucket_start(t), ts(2026, 3, 15, 13, 0));
        assert_eq!(Granularity::Day.bucket_start(t), ts(2026, 3, 15, 0, 0));
        assert_eq!(Granularity::Month.bucket_start(t), ts(2026, 3, 1, 0, 0));
    }

    #[test]
    fn bucket_end_is_exclusive_and_handles_month_and_year_rollover() {
        // The last instant of a bucket belongs to it; the end instant to the next.
        let last = ts(2026, 12, 31, 23, 59) + Duration::seconds(59);
        assert_eq!(Granularity::Month.bucket_start(last), ts(2026, 12, 1, 0, 0));
        assert_eq!(
            Granularity::Month.bucket_end(ts(2026, 12, 1, 0, 0)),
            ts(2027, 1, 1, 0, 0)
        );
        assert_eq!(
            Granularity::Month.bucket_end(ts(2028, 2, 1, 0, 0)),
            ts(2028, 3, 1, 0, 0)
        );
        assert_eq!(
            Granularity::Hour.bucket_start(ts(2027, 1, 1, 0, 0)),
            ts(2027, 1, 1, 0, 0)
        );
        assert_eq!(
            Granularity::Day.bucket_end(ts(2026, 12, 31, 0, 0)),
            ts(2027, 1, 1, 0, 0)
        );
    }

    #[test]
    fn current_bucket_is_incomplete_and_closed_bucket_is_complete() {
        let start = ts(2026, 3, 15, 13, 0);
        assert!(!Granularity::Hour.is_complete(start, ts(2026, 3, 15, 13, 59)));
        assert!(Granularity::Hour.is_complete(start, ts(2026, 3, 15, 14, 0)));
    }

    #[test]
    fn late_event_maps_back_to_its_closed_bucket() {
        // An event stamped 12:30 arriving at 14:10 belongs to the 12:00 bucket,
        // which `rollup_recent` (lookback >= 2) recomputes.
        let late = ts(2026, 3, 15, 12, 30);
        let now = ts(2026, 3, 15, 14, 10);
        let mut s = Granularity::Hour.bucket_start(now);
        let mut covered = vec![];
        for _ in 0..=2 {
            covered.push(s);
            s = Granularity::Hour.bucket_start(s - Duration::seconds(1));
        }
        assert!(covered.contains(&Granularity::Hour.bucket_start(late)));
    }
}
