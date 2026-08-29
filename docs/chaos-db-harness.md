# Database Chaos / Fault-Injection Harness

## Overview

`tests/chaos_db_harness_test.rs` is a reproducible chaos harness that injects
connection drops, latency spikes, and pool exhaustion at randomised points
across three representative request flows, then asserts data-consistency
invariants after each run.

## Running

```bash
# All chaos tests (requires Docker for testcontainers)
cargo test --test chaos_db_harness_test -- --ignored --nocapture

# Reproduce a specific failure from a known seed
CHAOS_SEED=12345 cargo test --test chaos_db_harness_test -- --ignored

# Single test
cargo test --test chaos_db_harness_test chaos_smoke_three_flows -- --ignored --nocapture
```

## CI (nightly job)

```yaml
chaos-db:
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
    - name: Run chaos harness
      run: cargo test --test chaos_db_harness_test -- --ignored --nocapture
      env:
        CHAOS_SEED: ${{ github.run_number }}
```

## Fault Modes

| Mode | Description |
|---|---|
| `ConnectionDrop` | Terminates all idle backend connections via `pg_terminate_backend`; validates that the pool reconnects transparently |
| `LatencySpike` | Injects 50–500 ms sleep on the application side; ensures timeouts fire correctly and leave no stuck state |
| `PoolExhaustion` | Holds 1–3 pool connections before the operation under test; validates clean fail-fast on `PoolTimedOut` |

Fault injection point is also randomised: before, during (between flows), or
after the flows under test.

## Flows Covered

| # | Flow | Tables Touched | Fault Surface |
|---|---|---|---|
| 1 | Webhook transaction ingestion | `transactions` (INSERT) | Write path, partitioned table |
| 2 | Settlement listing | `settlements` (SELECT) | Read / replica path, RLS sub-select |
| 3 | Reconciliation / DLQ | `transactions` (INSERT + SELECT FOR UPDATE + UPDATE) | Mixed read-write, explicit transaction |

## Data-Consistency Invariants

After every chaos round the harness queries `pg_stat_activity` and `pg_locks`
to assert:

1. **No partial writes** — every `transactions` row has non-null required fields
2. **No stuck advisory locks** — `pg_locks WHERE NOT granted AND state = 'idle in transaction'`
3. **No long-lived idle-in-transaction connections** — `state_change > 5 seconds`

Any violation fails the test immediately with a descriptive message including
the seed and round number.

## Reproducibility

All randomness is sourced from `StdRng::seed_from_u64(seed)`.  The seed
defaults to `0xDEAD_BEEF_CAFE_1234` and can be overridden via `CHAOS_SEED`.
A failing CI run can be reproduced locally with the same seed.

## Resilience Gaps Found

| Gap | Severity | Status |
|---|---|---|
| Pool reconnect after `pg_terminate_backend` is transparent — no gap found | — | ✅ Verified |
| `PoolTimedOut` under exhaustion propagates cleanly as `sqlx::Error` — no partial writes observed | — | ✅ Verified |
| Latency spikes do not leave idle-in-transaction connections beyond the 5s threshold | — | ✅ Verified |
| Settlements RLS policy uses a correlated sub-select on `transactions`; under high load this could be slow — not a correctness gap but a performance concern | Low | 📋 Follow-up: consider a `tenant_id` denorm on `settlements` when per-tenant settlement batching is adopted |

## Design Notes

- The harness does **not** require the full HTTP server; it tests the DB layer
  directly, which is the actual resilience boundary.
- A separate admin pool is used for invariant checks during pool-exhaustion
  tests to avoid the invariant assertions themselves failing due to the
  exhausted pool.
- The `set_admin_ctx` helper sets `app.is_admin = 'true'` on each connection
  so RLS policies allow the raw-pool inserts without a tenant API key.
