# Idempotency Keys in Authentication

This page used to describe a generic idempotency-key design with its own retention table (24 hours by default, 1 hour for short-lived operations, 7 days for long-running ones). That table did not match the implementation, so the two documents disagreed.

**The canonical document is [`docs/idempotency.md`](../../docs/idempotency.md).** It describes the behaviour of the code of record, `src/middleware/idempotency.rs` and `src/handlers/idempotency.rs`.

Key lifetime as implemented:

| Stage | Lifetime |
| ----- | -------- |
| Processing lock (`SET ... NX EX 300`) | 5 minutes |
| Completed response stored in Redis | 24 hours (`86400` seconds) |
| Postgres fallback row in `idempotency_keys` (used when Redis is unreachable) | 24 hours (`expires_at`) |

There are no per-operation retention classes (1 hour or 7 days); every key uses the values above. Keys are limited to `IDEMPOTENCY_KEY_MAX_LENGTH` (255) characters.
