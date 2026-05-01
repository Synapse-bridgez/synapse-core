CREATE TABLE IF NOT EXISTS reconciliation_reports (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    generated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    period_start TIMESTAMPTZ NOT NULL,
    period_end TIMESTAMPTZ NOT NULL,
    total_db_transactions INTEGER NOT NULL DEFAULT 0,
    total_chain_payments INTEGER NOT NULL DEFAULT 0,
    missing_on_chain_count INTEGER NOT NULL DEFAULT 0,
    orphaned_payments_count INTEGER NOT NULL DEFAULT 0,
    amount_mismatches_count INTEGER NOT NULL DEFAULT 0,
    has_discrepancies BOOLEAN NOT NULL DEFAULT false,
    report_json JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_reconciliation_reports_generated_at ON reconciliation_reports (generated_at DESC);
CREATE INDEX idx_reconciliation_reports_has_discrepancies ON reconciliation_reports (has_discrepancies) WHERE has_discrepancies = true;
