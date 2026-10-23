DROP TABLE IF EXISTS swap_orders;
DROP TABLE IF EXISTS deposit_callback_receipts;
ALTER TABLE transactions DROP CONSTRAINT IF EXISTS transactions_source_tenant_key;
