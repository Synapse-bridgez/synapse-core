# Chaos Engineering and Fault-Injection Testing (#1476)

This document describes the fault-injection strategy for the Synapse Core swap engine, covering each dependency failure mode, the injection mechanism, the expected resilience behavior, and how to run the chaos suite.

## Table of Contents

1. [Philosophy](#philosophy)
2. [Feature Flag Gate](#feature-flag-gate)
3. [Dependency Fault Taxonomy](#dependency-fault-taxonomy)
4. [Injection Mechanisms](#injection-mechanisms)
5. [Test Suites](#test-suites)
6. [Running Locally](#running-locally)
7. [CI Integration](#ci-integration)
8. [Determinism and Reproducibility](#determinism-and-reproducibility)
9. [What Each Test Asserts](#what-each-test-asserts)
10. [Adding a New Fault Mode](#adding-a-new-fault-mode)

---

## Philosophy

The swap engine is only as reliable as its behavior when dependencies misbehave. Chaos testing deliberately triggers every documented failure path — before production does — so that timeout, retry, and failover code is continuously exercised and regressions are caught in CI rather than during incidents.

The testing approach follows three rules:

1. **One fault at a time** by default, to make assertion failures unambiguous.
2. **Seeded randomness** for combined-fault scenarios so failures are reproducible.
3. **No production impact**: every injection seam compiles out in non-chaos builds.

---

## Feature Flag Gate

All fault-injection code is gated behind the `chaos` Cargo feature:

```toml
# Cargo.toml (dev-dependencies section)
[features]
chaos = []
```

The feature is **not** listed in `default = []` and therefore cannot be enabled in release builds through configuration alone. Every injection point uses `#[cfg(feature = "chaos")]`, so the compiled production binary contains zero chaos code.

**Enabling for a test run:**
```bash
cargo test --test chaos_swap_engine_test --features chaos -- --ignored --nocapture
```

---

## Dependency Fault Taxonomy

| Fault mode | Dependency | Simulated condition | Expected behavior |
|---|---|---|---|
| `DbConnectionLoss` | PostgreSQL | All idle connections terminated | Retry with reconnect; no data corruption |
| `DbHighLatency { millis }` | PostgreSQL | Application-side sleep before query | Degrade or fail fast (>1 s triggers timeout) |
| `RedisUnavailable` | Redis | Every cache op returns `Err` | Degrade: fall through to DB path |
| `OracleTimeout` | Price oracle | Snapshot age = 120 s | Circuit opens; quoting refused |
| `OracleStaleData` | Price oracle | Snapshot age > `PRICE_MAX_AGE_SECS` (15 s) | Degrade: serve last known quote, mark stale |
| `LpEndpointDown` | LP endpoint | TCP refused (flag-simulated) | Circuit opens; execution refused |
| `LpPartialResponse` | LP endpoint | Truncated body, retry once | Retry with backoff; second call succeeds |

### Documented Behaviors

- **RetryWithReconnect**: pool re-establishes the connection transparently; the caller sees at most one error before the retry succeeds.
- **RetryWithBackoff**: operation retries up to `PRICE_SOURCE_MAX_RETRIES` times with exponential backoff (`PRICE_SOURCE_RETRY_BACKOFF_MS`).
- **FailFast**: operation returns `Err` immediately when the configured timeout is exceeded, rather than waiting for a system-level TCP timeout.
- **DegradedPerformance**: throughput drops but orders continue to process; no stuck or orphaned rows.
- **DegradedFallthrough**: the primary path (cache / oracle) is bypassed; the engine uses a secondary source (DB / last snapshot) and continues.
- **CircuitOpen**: the circuit breaker in `src/swap/circuit_breaker.rs` opens; subsequent calls short-circuit with `SwapError::Unavailable` until the circuit resets.

---

## Injection Mechanisms

### Database

Connection loss is injected by running `pg_terminate_backend` against all idle backends in the same database:

```sql
SELECT pg_terminate_backend(pid)
FROM pg_stat_activity
WHERE datname = current_database()
  AND pid <> pg_backend_pid()
  AND state = 'idle';
```

The `sqlx` pool reconnects automatically; the test verifies that a write immediately after injection succeeds within one retry window.

High latency is simulated by a `tokio::time::sleep` on the application thread before the query, exercising the pool's `acquire_timeout` path without modifying the database server.

### Redis

`FailingRedisClient` (defined in `tests/chaos_swap_engine_test.rs`) implements the same get/set interface as the real Redis client but always returns `Err("Redis connection refused")`. It is injected by replacing the real client reference behind a trait object or `Arc` seam.

### Price Oracle

`StalePriceOracle` returns a `SyntheticPriceSnapshot` with a configurable `age_secs`. When `age_secs > PRICE_MAX_AGE_SECS` (15 s by default), `is_usable()` returns `false`, matching the production `PriceSnapshot::is_usable()` logic in `src/swap/quote.rs`.

### LP Endpoint

`MockLpEndpoint` exposes atomic `down` and `partial` flags. Setting `down = true` makes every call return `LpError::Unavailable`. Setting `partial = true` makes every call return `LpError::PartialResponse`. Flags are reset between rounds to keep faults single-shot unless the test explicitly chains them.

---

## Test Suites

### `tests/chaos_swap_engine_test.rs` (new — #1476)

Swap-domain-specific fault injection:

| Test name | Docker required | Covers |
|---|---|---|
| `test_db_connection_loss_reconnects_without_data_corruption` | Yes | `DbConnectionLoss` |
| `test_redis_unavailable_degrades_gracefully` | No | `RedisUnavailable` |
| `test_oracle_timeout_opens_circuit` | No | `OracleTimeout` |
| `test_oracle_stale_data_falls_through` | No | `OracleStaleData` |
| `test_lp_endpoint_down_opens_circuit` | No | `LpEndpointDown` |
| `test_lp_partial_response_retries_with_backoff` | No | `LpPartialResponse` |
| `test_combined_faults_produce_no_stuck_orders` | Yes | Random combined faults |
| `test_db_high_latency_completes_without_stuck_orders` | Yes | `DbHighLatency` |
| `fault_taxonomy_covers_all_required_scenarios` | No | Documentation contract |

### `tests/chaos_db_harness_test.rs` (existing)

Generic DB fault harness covering the transaction ingestion, settlement listing, and reconciliation paths. Run separately:

```bash
cargo test --test chaos_db_harness_test -- --ignored --nocapture
```

See `docs/chaos-db-harness.md` for the full description.

---

## Running Locally

**Prerequisites**: Docker (for the tests marked "Docker required"), Rust toolchain.

```bash
# All chaos tests (Docker required for --ignored subset)
cargo test --test chaos_swap_engine_test --features chaos -- --ignored --nocapture

# Non-Docker tests only (no `--ignored`)
cargo test --test chaos_swap_engine_test --features chaos --nocapture

# Generic DB harness
cargo test --test chaos_db_harness_test -- --ignored --nocapture

# Reproduce a specific seed
CHAOS_SEED=12345 cargo test --test chaos_swap_engine_test --features chaos -- --ignored
```

---

## CI Integration

Add to a nightly (or PR-gated) workflow:

```yaml
- name: Swap engine chaos suite
  run: |
    cargo test --test chaos_swap_engine_test --features chaos -- --ignored
    cargo test --test chaos_db_harness_test -- --ignored
  env:
    CHAOS_SEED: ${{ github.run_number }}
    RUST_LOG: info
```

Using `github.run_number` as the seed means every CI run exercises a different fault sequence while still being reproducible by seed.

---

## Determinism and Reproducibility

The `CHAOS_SEED` environment variable controls the `StdRng` seed used to pick fault modes and injection timing. The default value is `0xDEAD_CAFE` so the suite is deterministic in CI unless explicitly varied.

To reproduce a failing seed from CI:

```bash
CHAOS_SEED=<failing-seed> cargo test --test chaos_swap_engine_test --features chaos -- --ignored --nocapture
```

---

## What Each Test Asserts

After every fault round, invariants are checked:

1. **No partial swap orders**: `swap_orders` rows must have non-null `source_asset`, `source_amount`, and `status`.
2. **No stuck advisory locks**: `pg_locks` must have no un-granted locks from idle-in-transaction sessions.
3. **No idle-in-transaction connections**: no connection has been in `idle in transaction` state for more than 5 seconds.

These are the same invariants as the generic DB harness, extended to cover swap domain tables.

---

## Adding a New Fault Mode

1. Add a variant to the `SwapFaultMode` enum in `tests/chaos_swap_engine_test.rs`.
2. Map it to an `ExpectedBehavior` in `expected_behavior()`.
3. Add it to the `random()` picker so combined-fault rounds can select it.
4. Write a focused test that fires the fault and asserts the expected behavior.
5. Update this table in `docs/chaos-engineering.md`.
6. Open a PR with both the test and the table update.
