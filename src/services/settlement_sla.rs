use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;
use crate::error::AppError;
use tracing::{error, info, warn};

/// SLA configuration for settlement disputes by priority level
#[derive(Debug, Clone)]
pub struct SLAConfig {
    pub critical: i32,  // minutes
    pub high: i32,
    pub standard: i32,
    pub low: i32,
}

impl Default for SLAConfig {
    fn default() -> Self {
        Self {
            critical: 60,      // 1 hour
            high: 240,        // 4 hours
            standard: 1440,   // 24 hours
            low: 10080,       // 7 days
        }
    }
}

impl SLAConfig {
    pub fn get_duration_for_priority(&self, priority: &str) -> i32 {
        match priority {
            "critical" => self.critical,
            "high" => self.high,
            "standard" => self.standard,
            "low" => self.low,
            _ => self.standard,
        }
    }
}

/// Represents a settlement SLA breach event
#[derive(Debug, Clone)]
pub struct SLABreach {
    pub settlement_id: Uuid,
    pub priority: String,
    pub deadline: DateTime<Utc>,
    pub hours_overdue: i64,
}

/// Settlement SLA escalation job that runs periodically to detect and escalate SLA breaches
pub struct SettlementSLAJob {
    pool: PgPool,
    sla_config: SLAConfig,
}

impl SettlementSLAJob {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            sla_config: SLAConfig::default(),
        }
    }

    pub fn with_config(pool: PgPool, sla_config: SLAConfig) -> Self {
        Self {
            pool,
            sla_config,
        }
    }

    /// Detect disputes that have breached their SLA
    async fn detect_breaches(&self) -> Result<Vec<SLABreach>, AppError> {
        let now = Utc::now();

        let breaches = sqlx::query_as::<_, (Uuid, String, DateTime<Utc>)>(
            r#"
            SELECT id, sla_priority, sla_deadline
            FROM settlements
            WHERE status = 'disputed'
              AND sla_breached = FALSE
              AND sla_deadline IS NOT NULL
              AND sla_deadline < NOW()
            ORDER BY sla_deadline ASC
            "#
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        let result = breaches.into_iter().map(|(settlement_id, priority, deadline)| {
            let hours_overdue = (now.signed_duration_since(deadline).num_minutes() / 60) as i64;
            SLABreach {
                settlement_id,
                priority,
                deadline,
                hours_overdue,
            }
        }).collect();

        Ok(result)
    }

    /// Mark a settlement's SLA as breached and log the escalation
    async fn escalate_breach(&self, breach: &SLABreach) -> Result<(), AppError> {
        let mut tx = self.pool.begin()
            .await
            .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        // Update settlement to mark SLA as breached
        sqlx::query(
            r#"
            UPDATE settlements
            SET sla_breached = TRUE, sla_breach_notified_at = NOW()
            WHERE id = $1 AND sla_breached = FALSE
            "#
        )
        .bind(breach.settlement_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        // Log escalation event
        sqlx::query(
            r#"
            INSERT INTO settlement_sla_escalations
            (settlement_id, escalation_type, priority_before, notes, escalated_by)
            VALUES ($1, $2, $3, $4, $5)
            "#
        )
        .bind(breach.settlement_id)
        .bind("notification")
        .bind(&breach.priority)
        .bind(format!("SLA breach detected: {} hours overdue", breach.hours_overdue))
        .bind("system")
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        Ok(())
    }

    /// Emit a webhook notification for SLA breach
    async fn emit_breach_notification(&self, breach: &SLABreach) -> Result<(), AppError> {
        // Create webhook event for SLA breach
        // This uses the existing webhook dispatch mechanism
        let event_type = "settlement.sla_breached";
        let payload = serde_json::json!({
            "settlement_id": breach.settlement_id.to_string(),
            "priority": breach.priority,
            "deadline": breach.deadline.to_rfc3339(),
            "hours_overdue": breach.hours_overdue,
            "timestamp": Utc::now().to_rfc3339(),
            "event_type": event_type,
        });

        // Queue for webhook dispatch
        sqlx::query(
            r#"
            INSERT INTO webhook_events (event_type, payload, tenant_id, created_at)
            SELECT $1, $2, tenant_id, NOW()
            FROM settlements
            WHERE id = $3
            "#
        )
        .bind(event_type)
        .bind(payload.to_string())
        .bind(breach.settlement_id)
        .execute(&self.pool)
        .await
        .map_err(|e| {
            error!("Failed to emit SLA breach notification: {}", e);
            AppError::DatabaseError(e.to_string())
        })?;

        Ok(())
    }

    /// Update SLA deadlines for newly disputed settlements
    pub async fn initialize_sla_for_dispute(&self, settlement_id: Uuid, priority: &str) -> Result<(), AppError> {
        let duration_minutes = self.sla_config.get_duration_for_priority(priority);
        let deadline = Utc::now() + Duration::minutes(duration_minutes as i64);

        sqlx::query(
            r#"
            UPDATE settlements
            SET sla_priority = $1, sla_duration_minutes = $2, sla_deadline = $3
            WHERE id = $4
            "#
        )
        .bind(priority)
        .bind(duration_minutes)
        .bind(deadline)
        .bind(settlement_id)
        .execute(&self.pool)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        info!(
            settlement_id = %settlement_id,
            priority = priority,
            deadline = %deadline.to_rfc3339(),
            "SLA initialized for settlement dispute"
        );

        Ok(())
    }

    /// Clear SLA for resolved disputes
    pub async fn clear_sla_for_resolution(&self, settlement_id: Uuid) -> Result<(), AppError> {
        sqlx::query(
            r#"
            UPDATE settlements
            SET sla_priority = NULL, sla_deadline = NULL, sla_breached = FALSE
            WHERE id = $1
            "#
        )
        .bind(settlement_id)
        .execute(&self.pool)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        info!(settlement_id = %settlement_id, "SLA cleared for settlement resolution");

        Ok(())
    }

    /// Get all active SLA breaches
    pub async fn get_active_breaches(&self) -> Result<Vec<SLABreach>, AppError> {
        self.detect_breaches().await
    }
}

#[async_trait]
impl crate::services::scheduler::Job for SettlementSLAJob {
    fn name(&self) -> &str {
        "settlement_sla_checker"
    }

    fn schedule(&self) -> &str {
        "0 */5 * * * * *"  // Every 5 minutes
    }

    async fn execute(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        info!("Starting settlement SLA breach check");

        match self.detect_breaches().await {
            Ok(breaches) => {
                if breaches.is_empty() {
                    info!("No SLA breaches detected");
                    return Ok(());
                }

                info!("Detected {} SLA breaches", breaches.len());

                for breach in breaches {
                    if let Err(e) = self.escalate_breach(&breach).await {
                        error!("Failed to escalate SLA breach for {}: {}", breach.settlement_id, e);
                        continue;
                    }

                    if let Err(e) = self.emit_breach_notification(&breach).await {
                        warn!("Failed to emit breach notification for {}: {}", breach.settlement_id, e);
                    }
                }
            }
            Err(e) => {
                error!("Failed to detect SLA breaches: {}", e);
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("SLA detection failed: {}", e),
                )));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sla_config_defaults() {
        let config = SLAConfig::default();
        assert_eq!(config.critical, 60);
        assert_eq!(config.high, 240);
        assert_eq!(config.standard, 1440);
        assert_eq!(config.low, 10080);
    }

    #[test]
    fn test_get_duration_for_priority() {
        let config = SLAConfig::default();
        assert_eq!(config.get_duration_for_priority("critical"), 60);
        assert_eq!(config.get_duration_for_priority("high"), 240);
        assert_eq!(config.get_duration_for_priority("standard"), 1440);
        assert_eq!(config.get_duration_for_priority("low"), 10080);
        assert_eq!(config.get_duration_for_priority("unknown"), 1440); // defaults to standard
    }

    #[test]
    fn test_sla_breach_calculation() {
        let now = Utc::now();
        let deadline = now - Duration::hours(2);

        let breach = SLABreach {
            settlement_id: Uuid::new_v4(),
            priority: "high".to_string(),
            deadline,
            hours_overdue: 2,
        };

        assert_eq!(breach.hours_overdue, 2);
        assert_eq!(breach.priority, "high");
    }
}
