-- Rollback: remove v2 payment verification artefacts.
DROP INDEX IF EXISTS idx_transactions_pending_review;
DELETE FROM feature_flags WHERE name = 'payment_verification_v2';
ALTER TABLE transactions DROP COLUMN IF EXISTS verification_source;
