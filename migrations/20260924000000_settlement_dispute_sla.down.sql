-- Rollback SLA timer functionality
DROP INDEX IF EXISTS settlement_sla_escalations_type_idx;
DROP INDEX IF EXISTS settlement_sla_escalations_settlement_idx;
DROP TABLE IF EXISTS settlement_sla_escalations;

DROP INDEX IF EXISTS settlements_sla_breach_idx;
DROP INDEX IF EXISTS settlements_sla_check_idx;

ALTER TABLE settlements
    DROP COLUMN IF EXISTS sla_breach_notified_at,
    DROP COLUMN IF EXISTS sla_breached,
    DROP COLUMN IF EXISTS sla_deadline,
    DROP COLUMN IF EXISTS sla_duration_minutes,
    DROP COLUMN IF EXISTS sla_priority;
