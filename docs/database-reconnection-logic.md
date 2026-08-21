# Database Reconnection Logic

This document explains the database reconnection and retry strategy used by the Database module.

## Goal

Provide a secure and predictable strategy for recovering from transient PostgreSQL failures without leaking sensitive details or retrying permanent errors.

## Retry and Reconnection Strategy

The database layer relies on `sqlx` for connection pooling and query execution. Transient failures are handled explicitly using the retry utility in `src/utils/retry.rs`.

### Transient error classification

The helper `crate::utils::retry::is_transient_db_error` marks the following cases as retryable:

- `sqlx::Error::Io(_)`
- `sqlx::Error::PoolTimedOut`
- For `sqlx::Error::Database`, the PostgreSQL SQLSTATE code (`db_err.code()`) is
  checked first — codes such as `40P01` (deadlock), `40001` (serialization
  failure), `08000`/`08001`/`08003`/`08004`/`08006` (connection failures),
  `57P03` (cannot connect now), and `53300` (too many connections) are
  treated as transient
- Message-substring matching (`"deadlock detected"`, `"could not connect"`, …)
  is used **only** as a fallback when the driver doesn't surface a SQLSTATE
  code, since message text is locale- and version-dependent

Permanent errors such as `RowNotFound` or `PoolClosed` are not retried.

### Exponential backoff with decorrelated jitter

`crate::utils::retry::retry_with_backoff` provides:

- configurable `max_retries`
- base delay in milliseconds
- decorrelated jitter (`min(cap, random(base, prev_delay * 3))`) drawn from a
  real RNG (`rand::thread_rng()`) per call, instead of a wall-clock-derived
  offset — this avoids correlated "thundering herd" retries when many
  callers fail at the same instant (e.g. a primary failover)
- a hard cap at 10 seconds per retry
- retry metrics emitted via tracing

This lets database queries recover from brief outages or connection hiccups while preventing hot loops.

### Idempotency requirement

`retry_with_backoff` may invoke the wrapped operation more than once for the
same logical call, so it must only wrap idempotent operations: SELECTs,
upserts (`ON CONFLICT DO NOTHING`/`DO UPDATE`), or writes guarded by a stable
idempotency key. `db::queries::insert_transaction` wraps its INSERT in
`ON CONFLICT (id, created_at) DO NOTHING` keyed on the caller-generated
transaction id, and only writes the audit log entry when the row didn't
already exist — so a retry after a transient post-commit error returns the
existing row instead of duplicating the write or the audit trail.

## Connection Pool Behavior

The Database module provides pooled connections via `src/db/mod.rs` and `src/db/pool_manager.rs`.

### Pool creation

- `create_pool` builds a PostgreSQL pool with statement timeout and idle timeout configured from application settings.
- Each connection is warmed up with `SELECT 1` during startup.
- The warm-up step is intentionally fail-open: failures are logged but do not make pool creation brittle.

### Statement timeouts

- Read-tier queries use shorter timeouts.
- Write-tier queries use longer, but bounded timeouts.
- Timeouts are enforced at query execution, and timed-out connections are dropped to prevent slow queries from monopolizing the pool.

## Security Guidance

- Never log raw connection strings.
- Log only the query label, timeout tier, and retry metadata when a retry occurs.
- Sensitive values such as tokens, passwords, or tenant secrets must never appear in retry diagnostics.
- Enforce tenant context using `queries::set_tenant_context` so PostgreSQL row-level security remains effective.

## Usage

When writing new database code:

- Prefer `with_timeout(QueryTier::Read, ..., ...)` for SELECT queries.
- Prefer `with_timeout(QueryTier::Write, ..., ...)` for INSERT/UPDATE/DELETE operations.
- Use `crate::utils::retry::retry_with_backoff` for operations that may hit transient connection or serialization failures.
- Keep retry counts conservative and avoid retrying non-idempotent operations without explicit safeguards.

## Related Files

- `src/db/mod.rs`
- `src/db/queries.rs`
- `src/utils/retry.rs`
