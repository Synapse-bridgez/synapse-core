-- Create transaction_notes table for append-only transaction annotations
CREATE TABLE IF NOT EXISTS transaction_notes (
    id BIGSERIAL PRIMARY KEY,
    transaction_id UUID NOT NULL,
    admin_principal VARCHAR(255) NOT NULL,
    note_text TEXT NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (transaction_id) REFERENCES transactions(id) ON DELETE CASCADE,
    INDEX idx_transaction_id (transaction_id),
    INDEX idx_created_at (created_at)
) PARTITION BY RANGE (EXTRACT(EPOCH FROM created_at)) (
    PARTITION p_default VALUES LESS THAN (MAXVALUE)
);

-- RLS policy: Tenants can only see their own transaction notes
ALTER TABLE transaction_notes ENABLE ROW LEVEL SECURITY;

CREATE POLICY transaction_notes_tenant_isolation ON transaction_notes
    USING (
        transaction_id IN (
            SELECT id FROM transactions WHERE tenant_id = current_setting('app.tenant_id')::UUID
        )
    );

-- Create index for efficient note pagination
CREATE INDEX idx_transaction_notes_created_desc
    ON transaction_notes(transaction_id, created_at DESC);
