//! Database contract for webhook handlers.
//!
//! This module documents how webhook-facing handlers and services are expected
//! to use the database layer. The executable query helpers live primarily in
//! [`crate::db::queries`] and the dispatcher service; this file keeps the
//! database invariants in one rustdoc-visible place.
//!
//! ## Inbound callback persistence
//!
//! `POST /callback` and `POST /callback/transaction` validate external Anchor
//! Platform payloads before creating a [`crate::db::models::Transaction`].
//! The handler must then persist the row through
//! [`crate::db::queries::insert_transaction`], which:
//!
//! - stores the normalized payload in `transactions`;
//! - sets the initial status to `pending`;
//! - preserves Anchor callback metadata in `anchor_transaction_id`,
//!   `callback_type`, and `callback_status`;
//! - writes the transaction creation audit entry in the same SQL transaction;
//! - binds all external values through sqlx parameters.
//!
//! Handler code should not build ad hoc INSERT statements for inbound
//! callbacks. Centralizing persistence in the query layer keeps audit logging,
//! cache invalidation, statement timeouts, and future tenant/RLS handling
//! consistent.
//!
//! ## Idempotency and replay
//!
//! Webhook handlers may consult the idempotency fallback helpers in
//! [`crate::db::queries`] when an upstream idempotency key is available:
//!
//! - [`crate::db::queries::check_idempotency_key`]
//! - [`crate::db::queries::insert_idempotency_key`]
//! - [`crate::db::queries::update_idempotency_key_response`]
//!
//! These helpers use the `idempotency_keys` table and should be called before
//! replaying or reprocessing a request that might have already completed.
//! Admin replay records are written to `webhook_replay_history`; every replay
//! should also produce an audit entry so operators can reconstruct who changed
//! a transaction and why.
//!
//! ## Outgoing delivery tables
//!
//! Terminal transaction transitions enqueue outgoing webhook rows in
//! `webhook_deliveries` for enabled `webhook_endpoints`. The delivery table is
//! intentionally append/update oriented:
//!
//! - `webhook_deliveries.status` tracks `pending`, `in_progress`, `delivered`, or `failed`;
//!   `in_progress` means a worker has claimed the row (see `claimed_at`); workers
//!   reclaim stale `in_progress` rows older than `CLAIM_TIMEOUT_SECS` (default 5 min);
//! - `attempt_count`, `last_attempt_at`, and `next_attempt_at` support retry
//!   scheduling;
//! - `response_status` and `response_body` capture endpoint responses for
//!   diagnostics;
//! - `attempt_history` stores every attempt as a `JSONB` array of
//!   `{attempt, attempted_at, response_status, response_body, error}` objects,
//!   used for DLQ routing and operator inspection;
//! - `claimed_at` records when a worker claimed the row for processing; older
//!   `in_progress` rows are candidates for reclaim after a crash;
//! - the `(endpoint_id, transaction_id, event_type)` uniqueness constraint
//!   prevents duplicate delivery rows for the same event;
//! - exhausted deliveries (after `MAX_ATTEMPTS`) are inserted into
//!   `webhook_delivery_dlq` with the full `attempt_history` for replay;
//! - per-endpoint circuit breaker state is kept in Redis (`webhook_cb:<id>`);
//!   when open, the dispatcher skips and reschedules deliveries without
//!   consuming attempt budget.
//!
//! The dispatcher batches pending deliveries and endpoint lookups to avoid an
//! N+1 query pattern. New query paths should preserve that shape: select a
//! bounded batch, load related endpoints in bulk, then update individual
//! delivery rows by primary key.
//!
//! ## Security rules
//!
//! - Bind all request-controlled values with `.bind(...)` or
//!   `QueryBuilder::push_bind(...)`; only static SQL fragments may be pushed
//!   into dynamic statements.
//! - Never log webhook secrets, endpoint secrets, idempotency keys, or raw
//!   signature material. SQL labels passed to timeout helpers must stay
//!   sanitized and parameter-free.
//! - Keep multi-step state changes and audit writes in one database transaction
//!   where the caller needs atomic behavior.
//! - Enforce admin authorization before executing replay, health, endpoint
//!   mutation, or failed-webhook listing queries.
//! - Respect tenant context/RLS when a webhook query becomes tenant scoped.
//!
//! ## Performance assumptions
//!
//! The webhook query paths rely on these schema-level properties:
//!
//! - `webhook_deliveries.status` and partial `next_attempt_at` indexes make
//!   pending delivery scans bounded and scheduler friendly;
//! - `webhook_deliveries.endpoint_id` and `transaction_id` indexes support
//!   endpoint joins and transaction diagnostics;
//! - `webhook_replay_history.transaction_id`, `replayed_at`, and `success`
//!   indexes support replay audit views;
//! - admin list endpoints must apply explicit limits before querying and should
//!   prefer cursor pagination for new high-volume paths.
//!
//! ## Test coverage
//!
//! Relevant tests live in:
//!
//! - `tests/webhook_test.rs` for inbound validation and handler behavior;
//! - `tests/webhook_auth_test.rs` for signature verification;
//! - `tests/webhook_delivery_test.rs` for delivery retry/signature behavior;
//! - `tests/webhook_replay_test.rs` for replay tracking and status changes.
//!
//! Run `cargo test` before changing the database contract. Database-backed
//! tests that require `DATABASE_URL` may be ignored by default, but they should
//! still compile as part of the suite.
