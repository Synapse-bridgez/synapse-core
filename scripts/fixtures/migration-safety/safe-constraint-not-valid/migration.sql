-- Safe: two-step approach — add constraint without scanning, then validate separately
ALTER TABLE transactions ADD CONSTRAINT chk_amount_positive CHECK (amount > 0) NOT VALID;
-- Run in a subsequent migration or session:
-- ALTER TABLE transactions VALIDATE CONSTRAINT chk_amount_positive;
