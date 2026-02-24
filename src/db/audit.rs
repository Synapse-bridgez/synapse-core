use chrono::{DateTime, Utc};
use serde_json::{json, Value as JsonValue};
use sqlx::{Postgres, Transaction as SqlxTransaction};
use uuid::Uuid;

/// Entity type constants for audit logs
pub const ENTITY_TRANSACTION: &str = "transaction";
pub const ENTITY_SETTLEMENT: &str = "settlement";

/// Represents an audit log entry
#[derive(Debug, Clone)]
pub struct AuditLog {
    pub entity_id: Uuid,
    pub entity_type: String,
    pub action: String,
    pub old_val: Option<JsonValue>,
    pub new_val: Option<JsonValue>,
    pub actor: String,
    pub timestamp: DateTime<Utc>,
}

impl AuditLog {
    /// Create a new audit log entry
    pub fn new(
        entity_id: Uuid,
        entity_type: impl Into<String>,
        action: impl Into<String>,
        old_val: Option<JsonValue>,
        new_val: Option<JsonValue>,
        actor: impl Into<String>,
    ) -> Self {
        Self {
            entity_id,
            entity_type: entity_type.into(),
            action: action.into(),
            old_val,
            new_val,
            actor: actor.into(),
            timestamp: Utc::now(),
        }
    }

    /// Log an action with explicit old and new values
    pub async fn log(
        tx: &mut SqlxTransaction<'_, Postgres>,
        entity_id: Uuid,
        entity_type: &str,
        action: &str,
        old_val: Option<JsonValue>,
        new_val: Option<JsonValue>,
        actor: &str,
    ) -> sqlx::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO audit_logs (entity_id, entity_type, action, old_val, new_val, actor)
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(entity_id)
        .bind(entity_type)
        .bind(action)
        .bind(old_val)
        .bind(new_val)
        .bind(actor)
        .execute(&mut **tx)
        .await?;

        Ok(())
    }

    /// Log a status change
    pub async fn log_status_change(
        tx: &mut SqlxTransaction<'_, Postgres>,
        entity_id: Uuid,
        entity_type: &str,
        old_status: &str,
        new_status: &str,
        actor: &str,
    ) -> sqlx::Result<()> {
        Self::log(
            tx,
            entity_id,
            entity_type,
            "status_update",
            Some(json!({ "status": old_status })),
            Some(json!({ "status": new_status })),
            actor,
        )
        .await
    }

    /// Log a field update
    pub async fn log_field_update(
        tx: &mut SqlxTransaction<'_, Postgres>,
        entity_id: Uuid,
        entity_type: &str,
        field_name: &str,
        old_value: JsonValue,
        new_value: JsonValue,
        actor: &str,
    ) -> sqlx::Result<()> {
        Self::log(
            tx,
            entity_id,
            entity_type,
            &format!("{}_update", field_name),
            Some(json!({ field_name: old_value })),
            Some(json!({ field_name: new_value })),
            actor,
        )
        .await
    }

    /// Log a creation event
    pub async fn log_creation(
        tx: &mut SqlxTransaction<'_, Postgres>,
        entity_id: Uuid,
        entity_type: &str,
        created_data: JsonValue,
        actor: &str,
    ) -> sqlx::Result<()> {
        Self::log(
            tx,
            entity_id,
            entity_type,
            "created",
            None,
            Some(created_data),
            actor,
        )
        .await
    }

    /// Log a deletion event
    pub async fn log_deletion(
        tx: &mut SqlxTransaction<'_, Postgres>,
        entity_id: Uuid,
        entity_type: &str,
        deleted_data: JsonValue,
        actor: &str,
    ) -> sqlx::Result<()> {
        Self::log(
            tx,
            entity_id,
            entity_type,
            "deleted",
            Some(deleted_data),
            None,
            actor,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_log_creation() {
        let entity_id = Uuid::new_v4();
        let old_val = Some(json!({"status": "pending"}));
        let new_val = Some(json!({"status": "completed"}));

        let log = AuditLog::new(
            entity_id,
            ENTITY_TRANSACTION,
            "status_update",
            old_val.clone(),
            new_val.clone(),
            "system",
        );

        assert_eq!(log.entity_id, entity_id);
        assert_eq!(log.entity_type, ENTITY_TRANSACTION);
        assert_eq!(log.action, "status_update");
        assert_eq!(log.old_val, old_val);
        assert_eq!(log.new_val, new_val);
        assert_eq!(log.actor, "system");
    }
}
