-- Part E fix: process_batch now calls WebhookDispatcher::enqueue() on
-- transaction completion — previously nothing in the live app ever called
-- enqueue() at all, so webhook_deliveries never received a row and the
-- entire delivery subsystem (retry, circuit breaker, DLQ) sat idle.
--
-- Wiring that call back in also arms two previously-dead code paths in
-- webhook_dispatcher.rs (a synchronized-burst circuit-breaker-reset issue
-- and a rate-limiter TTL race), both fixed in this same migration/PR — but
-- as a further precaution this flag defaults OFF, so enabling delivery for
-- the first time in production is an explicit operator action, not an
-- automatic side effect of deploying this PR. Ramp rollout_percentage up by
-- stellar_account after enabling, monitoring webhook delivery success rate.
INSERT INTO feature_flags (name, enabled, description, rollout_percentage) VALUES
    ('webhook_enqueue_on_completion', false,
     'Enqueue an outbound webhook delivery when a transaction completes. Off by default — this is the first time this delivery pipeline has ever been armed in production; enable gradually via rollout_percentage.',
     0)
ON CONFLICT (name) DO NOTHING;
