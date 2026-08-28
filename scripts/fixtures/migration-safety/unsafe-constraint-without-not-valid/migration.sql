-- Unsafe: ADD CONSTRAINT without NOT VALID holds a full table scan lock
ALTER TABLE transactions ADD CONSTRAINT chk_amount_positive CHECK (amount > 0);
