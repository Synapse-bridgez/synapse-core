-- Fix a real bug: route_to_dlq() in src/services/webhook_dispatcher.rs inserts
-- into webhook_delivery_dlq with `ON CONFLICT (delivery_id) DO NOTHING`, but
-- delivery_id had no unique constraint or index backing that clause. Postgres
-- rejects such an INSERT outright with "there is no unique or exclusion
-- constraint matching the ON CONFLICT specification" (42P10), so *every*
-- webhook delivery that exhausted its retry attempts silently failed to be
-- recorded in the DLQ — the error was swallowed by the dispatcher's
-- fire-and-forget error logging in process_pending().
ALTER TABLE webhook_delivery_dlq
    ADD CONSTRAINT webhook_delivery_dlq_delivery_id_key UNIQUE (delivery_id);
