-- Part A fix: process_batch now verifies a specific matching Horizon
-- payment (amount/asset/memo) before completing a transaction, instead of
-- completing on no evidence at all (the live path made no Horizon call
-- whatsoever prior to this migration). See services/processor.rs's
-- module doc on process_batch for the verification contract.
--
-- Given the financial-correctness severity of changing what gates a live
-- payment-processor completion path, cutover defaults OFF (0% rollout).
-- While off, process_batch still evaluates the verification logic in
-- shadow mode and logs/counts divergences
-- (payment_verification_no_match_completed_total) without acting on them.
-- Ramp rollout_percentage up by stellar_account after reviewing shadow
-- divergence data.
INSERT INTO feature_flags (name, enabled, description, rollout_percentage) VALUES
    ('payment_verification_enabled', false,
     'Gate transaction completion in process_batch on a genuine matching Horizon payment (amount/asset/memo) rather than completing unconditionally. Off by default — shadow-evaluated first; ramp gradually via rollout_percentage after reviewing payment_verification_no_match_completed_total.',
     0)
ON CONFLICT (name) DO NOTHING;
