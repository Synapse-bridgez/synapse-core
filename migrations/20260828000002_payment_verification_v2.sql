-- #1097: Payment verification v2 — two-source cross-check
--
-- Adds a `verification_source` column to record which signals were checked,
-- a feature flag for the v2 path, and the `pending_review` status entry to
-- support explicit disagreement routing.
--
-- Design (per docs/adr/004-payment-matching-authority.md):
--   Signal 1 – Horizon payment lookup (existing, gated by payment_verification_enabled)
--   Signal 2 – Anchor callback received (anchor_transaction_id IS NOT NULL AND
--               callback_status = 'completed')
--
--   both agree  → completed  (normal happy path)
--   disagreement → pending_review  (routes to manual ops review)
--   only signal 1 present and signal 2 missing → leave pending (await anchor callback)
--   only signal 2 present and signal 1 missing → leave pending (await Horizon confirmation)
--
-- Rollout: flag defaults to 0% rollout. Shadow-evaluate first.

-- Record which verification signals were used when completing or routing a txn.
ALTER TABLE transactions
    ADD COLUMN IF NOT EXISTS verification_source VARCHAR(64);

-- Feature flag for the v2 two-source cross-check path.
INSERT INTO feature_flags (name, enabled, description, rollout_percentage) VALUES
    ('payment_verification_v2',
     false,
     'Enable two-source payment verification: both Horizon payment data and anchor callback \
must agree before completing a transaction. Disagreement routes to pending_review for manual \
ops triage rather than silently accepting or rejecting. Requires payment_verification_enabled=true \
to be set; this flag layers additional cross-check logic on top of it. Ramp gradually after \
reviewing payment_verification_no_match_completed_total and anchor callback arrival rates.',
     0)
ON CONFLICT (name) DO NOTHING;

-- Index to make the pending-review queue fast to query.
CREATE INDEX IF NOT EXISTS idx_transactions_pending_review
    ON transactions(status, updated_at)
    WHERE status = 'pending_review';
