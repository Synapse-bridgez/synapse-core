# Outbound Webhook Delivery Architecture

## Overview

This describes the **outbound** webhook subsystem (`src/services/webhook_dispatcher.rs`) — notifying tenant-registered endpoints when a transaction completes. This is distinct from the *inbound* webhook handler (`docs/webhook-handler.md`), which receives fiat deposit callbacks from the Stellar Anchor Platform.

Until this PR, no code path in the live application ever called `WebhookDispatcher::enqueue()`, so this entire subsystem — despite being fully implemented, tested in isolation, and polled every 30 seconds by a live background task — never actually delivered a webhook in production. See Part E of the accompanying PR. This document exists because that gap went unnoticed partly *because* no single doc described the lifecycle end to end.

## Lifecycle

```
transaction completes (processor.rs::process_batch, inside the row-locked batch transaction)
  │
  ▼
feature flag check: webhook_enqueue_on_completion (FeatureFlagService::is_enabled_for_key,
  keyed by stellar_account — off by default, see migration 20260823000002)
  │  (if disabled, nothing happens — this is the safety switch for gradual rollout)
  ▼
WebhookDispatcher::enqueue(transaction_id, "transaction.completed", payload)
  │  looks up enabled webhook_endpoints subscribed to the event type + matching filter_rules
  │  INSERT INTO webhook_deliveries (status='pending') ON CONFLICT (endpoint_id, transaction_id,
  │  event_type) DO NOTHING  — exactly-once enqueue per (endpoint, transaction, event)
  ▼
background poll loop (main.rs, every 30s): WebhookDispatcher::process_pending()
  │  claims up to 100 due rows via FOR UPDATE SKIP LOCKED (also reclaims stuck
  │  in_progress rows older than CLAIM_TIMEOUT_SECS — crash recovery)
  │  groups claimed deliveries by endpoint_id
  ▼
per-endpoint circuit breaker check (circuit_breaker_decision)
  │  Closed        → all claimed deliveries for this endpoint proceed
  │  Open           → all rescheduled (jittered next_attempt_at around the reset
  │                    window, not the identical instant) without consuming an attempt
  │  HalfOpenProbe   → exactly one delivery proceeds as the probe (this caller won the
  │                    Redis SET NX PX lease); the rest are rescheduled like Open
  ▼
attempt_delivery_with_endpoint (concurrency-bounded via buffer_unordered, default 10)
  │  per-endpoint rate limit check (check_rate_limit — atomic Redis Lua INCR+EXPIRE)
  │    not allowed → reschedule to next cycle, no attempt consumed
  │  send_webhook: HMAC-SHA256-signed POST, records webhook_delivery_total /
  │    webhook_delivery_duration_ms metrics
  ├─ success → circuit_breaker_succeeded (resets breaker + releases probe lease)
  │             delivery marked 'delivered'
  └─ failure → circuit_breaker_failed (may trip breaker open; emits
                webhook_circuit_breaker_transitions_total{transition="opened"})
              handle_failure:
                attempt_count < MAX_ATTEMPTS (5) → reschedule with jittered
                  exponential backoff (BASE_DELAY_SECS * 2^attempt, ±25% jitter)
                attempt_count >= MAX_ATTEMPTS   → status='failed', routed to
                  webhook_delivery_dlq (see "DLQ replay" below)
```

## Circuit breaker

Redis-backed, keyed per endpoint (`webhook_cb:<endpoint_id>`), shared across all instances (unlike the separate, unused `CircuitBreaker` type in `circuit_breaker.rs`, whose "half-open" state is an in-process mutex guard with no cross-instance coordination and whose Redis persistence is write-only). States:

- **Closed**: no `webhook_cb:<endpoint_id>` key, or `state != "open"`.
- **Open**: `state == "open"` and less than `CB_RESET_TIMEOUT_SECS` (300s) has elapsed since `opened_at`. All deliveries for the endpoint reschedule.
- **Half-open, probing**: past the reset timeout. The first caller to `SET webhook_cb_probe:<endpoint_id> 1 NX PX 30000` wins the lease and gets `HalfOpenProbe` (exactly one delivery goes out); every other concurrent caller gets `Open` (reschedule) until the probe's result is recorded via `circuit_breaker_succeeded`/`circuit_breaker_failed`.

Trips open after `CB_FAILURE_THRESHOLD` (3) consecutive failures (tracked via an atomic Lua read-modify-write on the same key, so it persists across the open period rather than resetting per-attempt).

## Rate limiting

Per-endpoint, `max_delivery_rate` requests per 60s window, tracked via `webhook_rate:<endpoint_id>` in Redis. `INCR` + conditional `EXPIRE` happen inside a single atomic Lua script (`RATE_LIMIT_INCREMENT_SCRIPT`, same pattern as `middleware/quota.rs`), so a crash between the two can't leave a counter permanently stuck with no TTL — the script self-heals it on the next check and increments `webhook_rate_limit_self_healed_total`.

## Rollout

`webhook_enqueue_on_completion` (see `FeatureFlagService::is_enabled_for_key`, keyed by `stellar_account`) gates whether `enqueue()` is ever called at all. It ships **disabled** by default (migration `20260823000002`) — enabling it for the first time arms three previously-dead code paths simultaneously (enqueue, the circuit breaker, the rate limiter), so ramp `rollout_percentage` up gradually rather than flipping it to 100% fleet-wide in one deploy. Monitor `webhook_delivery_total{outcome=...}`, `webhook_circuit_breaker_transitions_total`, and `webhook_rate_limit_self_healed_total` while ramping.

## DLQ replay

See the runbook's "Webhook Delivery DLQ Review" section for the operational procedure.

## Known gaps

- `replay_from_dlq()` exists but has no HTTP admin endpoint — today it's only callable from a Rust context (e.g. a one-off binary or REPL against the running pool). Adding an admin route is a reasonable follow-up, out of scope for this PR.
- The legacy `run_processor` / `run_processor_with_leader_election` / `TransactionProcessorJob` code paths pass `webhook_dispatcher: None` — they don't enqueue deliveries even when reached. None of them are currently invoked from `main.rs`; `ProcessorPool::start()` is the live path and does have a dispatcher attached.
