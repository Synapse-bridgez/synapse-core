-- Also fixes (aside, discovered while getting Part E's tests green): route_to_dlq's
-- INSERT INTO webhook_delivery_dlq uses "ON CONFLICT (delivery_id) DO NOTHING", but
-- no unique constraint or index has ever existed on delivery_id. Postgres rejects
-- ON CONFLICT against a column with no matching unique constraint at query time
-- ("there is no unique or exclusion constraint matching the ON CONFLICT
-- specification"), so every single call to route_to_dlq has always failed —
-- silently, since the error only surfaces as a logged tracing::error! several
-- layers up, not a panic or a surfaced result. This means exhausted webhook
-- deliveries have never actually reached the DLQ table in this codebase's
-- history; the error was invisible for the same reason Part E.1 was invisible —
-- nothing exercised this path against a live delivery until enqueue() was wired
-- up (this PR).

CREATE UNIQUE INDEX IF NOT EXISTS idx_webhook_delivery_dlq_delivery_id_unique
    ON webhook_delivery_dlq (delivery_id);
