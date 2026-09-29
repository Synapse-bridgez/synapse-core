-- Drop RLS policies and transaction_notes table
DROP POLICY IF EXISTS transaction_notes_tenant_isolation ON transaction_notes;
DROP TABLE IF EXISTS transaction_notes;
