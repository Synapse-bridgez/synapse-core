-- Account monitor cursor tracking
CREATE TABLE IF NOT EXISTS account_monitor_cursors (
    account VARCHAR(56) PRIMARY KEY,
    cursor TEXT NOT NULL,
    updated_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_account_monitor_updated ON account_monitor_cursors(updated_at);
