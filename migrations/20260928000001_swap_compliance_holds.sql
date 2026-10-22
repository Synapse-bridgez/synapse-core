-- #1529: Compliance hold, review queue and decision records for flagged swaps.
-- Workflow logic lives in src/swap/compliance.rs.

CREATE TABLE IF NOT EXISTS swap_compliance_holds (
    id            UUID        PRIMARY KEY,
    order_id      UUID        NOT NULL,
    status        TEXT        NOT NULL DEFAULT 'held'
                  CHECK (status IN ('held', 'under_review', 'released', 'rejected', 'voided')),
    risk          TEXT        NOT NULL DEFAULT 'standard' CHECK (risk IN ('standard', 'high')),
    reason        TEXT        NOT NULL,
    assignee      TEXT,
    escalated     BOOLEAN     NOT NULL DEFAULT FALSE,
    opened_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    sla_deadline  TIMESTAMPTZ NOT NULL,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- At most one open hold per order.
CREATE UNIQUE INDEX IF NOT EXISTS idx_swap_compliance_holds_open_order
    ON swap_compliance_holds (order_id) WHERE status IN ('held', 'under_review');
CREATE INDEX IF NOT EXISTS idx_swap_compliance_holds_queue
    ON swap_compliance_holds (sla_deadline) WHERE status IN ('held', 'under_review');

CREATE TABLE IF NOT EXISTS swap_compliance_decisions (
    id               BIGSERIAL   PRIMARY KEY,
    hold_id          UUID        NOT NULL REFERENCES swap_compliance_holds(id) ON DELETE CASCADE,
    reviewer_id      TEXT        NOT NULL,
    kind             TEXT        NOT NULL CHECK (kind IN ('release', 'reject')),
    reason_code      TEXT        NOT NULL,
    idempotency_key  TEXT        NOT NULL,
    decided_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (hold_id, idempotency_key)
);
