use crate::db::models::ComplianceReport;
use crate::error::AppError;
use bigdecimal::BigDecimal;
use chrono::{DateTime, Datelike, Duration, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub struct ComplianceService {
    pool: PgPool,
}

impl ComplianceService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn generate_report(&self, period: &str) -> Result<ComplianceReport, AppError> {
        let now = Utc::now();
        let (period_start, period_end) = period_bounds(period, now)?;
        self.generate_for_range(period, period_start, period_end)
            .await
    }

    async fn generate_for_range(
        &self,
        period: &str,
        period_start: DateTime<Utc>,
        period_end: DateTime<Utc>,
    ) -> Result<ComplianceReport, AppError> {
        let db_err = |e: sqlx::Error| AppError::DatabaseError(e.to_string());

        // Transaction count and settlement total
        let summary = sqlx::query(
            "SELECT COUNT(*) AS tx_count, COALESCE(SUM(amount), 0) AS settlement_total \
             FROM transactions WHERE created_at >= $1 AND created_at < $2",
        )
        .bind(period_start)
        .bind(period_end)
        .fetch_one(&self.pool)
        .await
        .map_err(db_err)?;

        let tx_count: i64 = summary.try_get("tx_count").unwrap_or(0);
        let settlement_total: BigDecimal = summary
            .try_get::<BigDecimal, _>("settlement_total")
            .unwrap_or_default();

        // Anomaly count: transactions stuck in 'pending' for > 1 hour
        let anomaly_row = sqlx::query(
            "SELECT COUNT(*) AS anomaly_count FROM transactions \
             WHERE created_at >= $1 AND created_at < $2 \
               AND status = 'pending' AND created_at < NOW() - INTERVAL '1 hour'",
        )
        .bind(period_start)
        .bind(period_end)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        let anomaly_count: i64 = anomaly_row.try_get("anomaly_count").unwrap_or(0);

        // Volume by asset
        let asset_rows = sqlx::query(
            "SELECT asset_code, COUNT(*) AS tx_count, COALESCE(SUM(amount), 0) AS total_amount \
             FROM transactions WHERE created_at >= $1 AND created_at < $2 \
             GROUP BY asset_code",
        )
        .bind(period_start)
        .bind(period_end)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        let volume_by_asset: serde_json::Value = asset_rows
            .into_iter()
            .map(|r| {
                let code: String = r.try_get("asset_code").unwrap_or_default();
                let count: i64 = r.try_get("tx_count").unwrap_or(0);
                let amount: BigDecimal = r.try_get("total_amount").unwrap_or_default();
                (
                    code,
                    serde_json::json!({
                        "tx_count": count,
                        "total_amount": amount.to_string(),
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>()
            .into();

        // Top 10 accounts by volume
        let account_rows = sqlx::query(
            "SELECT stellar_account, COUNT(*) AS tx_count, COALESCE(SUM(amount), 0) AS total_amount \
             FROM transactions WHERE created_at >= $1 AND created_at < $2 \
             GROUP BY stellar_account ORDER BY total_amount DESC LIMIT 10",
        )
        .bind(period_start)
        .bind(period_end)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        let top_accounts: serde_json::Value = account_rows
            .into_iter()
            .map(|r| {
                let account: String = r.try_get("stellar_account").unwrap_or_default();
                let count: i64 = r.try_get("tx_count").unwrap_or(0);
                let amount: BigDecimal = r.try_get("total_amount").unwrap_or_default();
                serde_json::json!({
                    "stellar_account": account,
                    "tx_count": count,
                    "total_amount": amount.to_string(),
                })
            })
            .collect::<Vec<_>>()
            .into();

        let report = ComplianceReport {
            id: Uuid::new_v4(),
            period: period.to_string(),
            period_start,
            period_end,
            transaction_count: tx_count,
            settlement_total,
            anomaly_count,
            volume_by_asset,
            top_accounts,
            created_at: Utc::now(),
        };

        let saved = sqlx::query_as::<_, ComplianceReport>(
            "INSERT INTO compliance_reports \
                (id, period, period_start, period_end, transaction_count, \
                 settlement_total, anomaly_count, volume_by_asset, top_accounts, created_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING *",
        )
        .bind(report.id)
        .bind(&report.period)
        .bind(report.period_start)
        .bind(report.period_end)
        .bind(report.transaction_count)
        .bind(&report.settlement_total)
        .bind(report.anomaly_count)
        .bind(&report.volume_by_asset)
        .bind(&report.top_accounts)
        .bind(report.created_at)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        Ok(saved)
    }

    pub async fn list_reports(
        &self,
        period: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ComplianceReport>, AppError> {
        let reports = if let Some(p) = period {
            sqlx::query_as::<_, ComplianceReport>(
                "SELECT * FROM compliance_reports WHERE period = $1 \
                 ORDER BY period_start DESC LIMIT $2 OFFSET $3",
            )
            .bind(p)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query_as::<_, ComplianceReport>(
                "SELECT * FROM compliance_reports ORDER BY period_start DESC LIMIT $1 OFFSET $2",
            )
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        Ok(reports)
    }

    /// Get a specific report by ID
    pub async fn get_report(&self, report_id: Uuid) -> Result<ComplianceReport, AppError> {
        sqlx::query_as::<_, ComplianceReport>(
            "SELECT * FROM compliance_reports WHERE id = $1",
        )
        .bind(report_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?
        .ok_or_else(|| AppError::NotFound("Compliance report not found".to_string()))
    }

    /// Approve a compliance report (reviewer sign-off)
    pub async fn approve_report(
        &self,
        report_id: Uuid,
        reviewer_id: Uuid,
        reviewer_notes: Option<String>,
    ) -> Result<ComplianceReport, AppError> {
        // Check if report exists and is in pending_review status
        let report = self.get_report(report_id).await?;

        if let Some(ref status) = report.status {
            if status != "pending_review" {
                return Err(AppError::Validation(
                    format!("Cannot approve a report with status: {}", status),
                ));
            }
        }

        // Audit log the approval
        self.log_audit(
            report_id,
            "compliance_report_approved",
            Some(serde_json::json!({
                "reviewer_id": reviewer_id,
                "notes": reviewer_notes
            })),
        )
        .await
        .map_err(|e| {
            tracing::error!("Failed to log compliance report approval audit: {}", e);
            e
        })?;

        // Update the report status
        sqlx::query_as::<_, ComplianceReport>(
            "UPDATE compliance_reports
             SET status = 'approved',
                 reviewed_by = $2,
                 reviewed_at = $3,
                 reviewer_notes = $4,
                 updated_at = NOW()
             WHERE id = $1
             RETURNING *",
        )
        .bind(report_id)
        .bind(reviewer_id)
        .bind(Utc::now())
        .bind(reviewer_notes)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))
    }

    /// Reject a compliance report (reviewer rejects)
    pub async fn reject_report(
        &self,
        report_id: Uuid,
        reviewer_id: Uuid,
        reason: String,
    ) -> Result<ComplianceReport, AppError> {
        // Check if report exists and is in pending_review status
        let report = self.get_report(report_id).await?;

        if let Some(ref status) = report.status {
            if status != "pending_review" {
                return Err(AppError::Validation(
                    format!("Cannot reject a report with status: {}", status),
                ));
            }
        }

        // Audit log the rejection
        self.log_audit(
            report_id,
            "compliance_report_rejected",
            Some(serde_json::json!({
                "reviewer_id": reviewer_id,
                "reason": reason
            })),
        )
        .await
        .map_err(|e| {
            tracing::error!("Failed to log compliance report rejection audit: {}", e);
            e
        })?;

        // Update the report status
        sqlx::query_as::<_, ComplianceReport>(
            "UPDATE compliance_reports
             SET status = 'rejected',
                 reviewed_by = $2,
                 reviewed_at = $3,
                 reviewer_notes = $4,
                 updated_at = NOW()
             WHERE id = $1
             RETURNING *",
        )
        .bind(report_id)
        .bind(reviewer_id)
        .bind(Utc::now())
        .bind(Some(reason))
        .fetch_one(&self.pool)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))
    }

    /// Invalidate report approval when report is regenerated
    pub async fn invalidate_approvals_for_period(&self, period: &str) -> Result<u64, AppError> {
        let result = sqlx::query(
            "UPDATE compliance_reports
             SET status = 'pending_review',
                 reviewed_by = NULL,
                 reviewed_at = NULL,
                 reviewer_notes = NULL,
                 updated_at = NOW()
             WHERE period = $1 AND status = 'approved'",
        )
        .bind(period)
        .execute(&self.pool)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        Ok(result.rows_affected())
    }

    async fn log_audit(
        &self,
        report_id: Uuid,
        action: &str,
        metadata: Option<serde_json::Value>,
    ) -> Result<(), AppError> {
        // This uses the audit logging infrastructure - delegate to audit service
        // For now, just log to tracing
        tracing::info!(
            action = action,
            report_id = %report_id,
            metadata = ?metadata,
            "Compliance report audit event"
        );
        Ok(())
    }
}

fn period_bounds(
    period: &str,
    now: DateTime<Utc>,
) -> Result<(DateTime<Utc>, DateTime<Utc>), AppError> {
    match period {
        "daily" => {
            let start = now.date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc();
            Ok((start, start + Duration::days(1)))
        }
        "weekly" => {
            let days_from_monday = now.weekday().num_days_from_monday() as i64;
            let start = (now - Duration::days(days_from_monday))
                .date_naive()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc();
            Ok((start, start + Duration::weeks(1)))
        }
        "monthly" => {
            let start = now
                .date_naive()
                .with_day(1)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc();
            let next = if start.month() == 12 {
                start
                    .with_year(start.year() + 1)
                    .unwrap()
                    .with_month(1)
                    .unwrap()
            } else {
                start.with_month(start.month() + 1).unwrap()
            };
            Ok((start, next))
        }
        _ => Err(AppError::Validation(
            "period must be daily, weekly, or monthly".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_daily_bounds() {
        let now = Utc.with_ymd_and_hms(2026, 4, 28, 15, 30, 0).unwrap();
        let (start, end) = period_bounds("daily", now).unwrap();
        assert_eq!(start, Utc.with_ymd_and_hms(2026, 4, 28, 0, 0, 0).unwrap());
        assert_eq!(end, Utc.with_ymd_and_hms(2026, 4, 29, 0, 0, 0).unwrap());
    }

    #[test]
    fn test_weekly_bounds() {
        // 2026-04-28 is a Tuesday (days_from_monday = 1)
        let now = Utc.with_ymd_and_hms(2026, 4, 28, 10, 0, 0).unwrap();
        let (start, end) = period_bounds("weekly", now).unwrap();
        assert_eq!(start, Utc.with_ymd_and_hms(2026, 4, 27, 0, 0, 0).unwrap());
        assert_eq!(end, Utc.with_ymd_and_hms(2026, 5, 4, 0, 0, 0).unwrap());
    }

    #[test]
    fn test_monthly_bounds() {
        let now = Utc.with_ymd_and_hms(2026, 4, 15, 0, 0, 0).unwrap();
        let (start, end) = period_bounds("monthly", now).unwrap();
        assert_eq!(start, Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap());
        assert_eq!(end, Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn test_invalid_period() {
        let now = Utc::now();
        assert!(period_bounds("yearly", now).is_err());
    }
}
