-- Add SLA timer functionality to settlement disputes
ALTER TABLE settlements
    ADD COLUMN IF NOT EXISTS sla_priority VARCHAR(20) DEFAULT 'standard' CHECK (sla_priority IN ('critical', 'high', 'standard', 'low')),
    ADD COLUMN IF NOT EXISTS sla_duration_minutes INT DEFAULT 1440,  -- 24 hours default
    ADD COLUMN IF NOT EXISTS sla_deadline TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS sla_breached BOOLEAN DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS sla_breach_notified_at TIMESTAMPTZ;

-- Index for efficient SLA breach detection
CREATE INDEX IF NOT EXISTS settlements_sla_check_idx
ON settlements(sla_deadline, sla_priority, status, sla_breached)
WHERE status = 'disputed' AND sla_breached = FALSE;

-- Index for SLA breach escalation queries
CREATE INDEX IF NOT EXISTS settlements_sla_breach_idx
ON settlements(sla_breach_notified_at, sla_priority)
WHERE sla_breached = TRUE AND sla_breach_notified_at IS NULL;

-- Audit log for SLA escalation events
CREATE TABLE IF NOT EXISTS settlement_sla_escalations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    settlement_id UUID NOT NULL REFERENCES settlements(id) ON DELETE CASCADE,
    escalation_type VARCHAR(50) NOT NULL, -- 'notification', 'reassignment', 'escalation'
    priority_before VARCHAR(20) NOT NULL,
    priority_after VARCHAR(20),
    escalated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    escalated_by VARCHAR(100) NOT NULL DEFAULT 'system',
    notes TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Index for escalation history queries
CREATE INDEX IF NOT EXISTS settlement_sla_escalations_settlement_idx
ON settlement_sla_escalations(settlement_id, escalated_at DESC);

CREATE INDEX IF NOT EXISTS settlement_sla_escalations_type_idx
ON settlement_sla_escalations(escalation_type, escalated_at DESC);
