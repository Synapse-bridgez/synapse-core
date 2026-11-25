# Swap Engine Performance Benchmarks and Load Testing (#1459)

Status: **design note / not yet implemented.** This document is the plan for
issue #1459. It exists because the swap hot paths named in the issue
(quote aggregation, multi-hop routing, fee computation, order persistence) do
not exist in this repository yet, so there is nothing to benchmark.

## Problem

Issue #1459 asks for:

1. Criterion benchmarks for quote aggregation, multi-hop routing, fee
   computation, and order persistence.
2. A k6-style load-test scenario driving the quote and order REST endpoints.
3. Documented SLOs (p50/p99 quote latency, sustained order throughput) with
   hardware assumptions.
4. A CI/nightly job that runs the benchmarks and flags regressions beyond a
   configurable percentage against a stored baseline.
5. Reproducibility on a clean checkout, plus a reduced-size smoke test of the
   harness.

## Why it cannot be built yet

- `src/swap/mod.rs` is explicitly a **Phase 1 hand-off contract only**: it
  defines `SwapHandoff`, the `SwapEngine` trait, and `NoopSwapEngine`. Its own
  module docs state it "contains no swap or DEX-routing logic".
- There is no quote aggregation, routing, or fee-computation code anywhere in
  `src/` (grep for `quote`/`routing`/`fee` finds only unrelated matches).
- There are no quote/order REST endpoints in `src/lib.rs`'s router, so a load
  test would have nothing to drive.
- `src/swap/backfill.rs` and `src/db/backfill_queries.rs` reference
  `SwapQuote`/`SwapOrder` models that are not present in `src/db/models.rs`,
  and `src/swap` is not declared in `src/lib.rs` at all (see
  `docs/known-unreachable-modules.md`). The swap domain is not compiled into
  the crate today.

Writing benchmarks against invented APIs would encode guesses as a baseline
and produce a regression gate that measures nothing real.

## Approach (once the swap engine lands)

### 1. Benchmarks — `benches/swap_engine.rs`

Follow the existing `benches/critical_paths.rs` conventions
(`criterion_group!`/`criterion_main!`, `black_box`, `harness = false`).

| Benchmark id | Target | Shape |
|---|---|---|
| `swap_quote_aggregation` | aggregating N venue quotes into one best quote | `bench_with_input` over N = 1, 5, 20 |
| `swap_multi_hop_routing` | path search over a fixed synthetic graph | `bench_with_input` over hop count 1, 2, 3 |
| `swap_fee_computation` | fee + slippage math on a `BigDecimal` amount | `bench_function`, fixed representative amount |
| `swap_order_persistence` | order insert path | `iter_batched` against a testcontainers Postgres, or a pure mapping/encode benchmark if the insert is I/O-bound |

Register in `Cargo.toml`:

```toml
[[bench]]
name = "swap_engine"
harness = false
```

### 2. Load test — `loadtest/swap.js` (k6)

- `setup()` seeds representative data (accounts, assets, order rows) via the
  REST API or a SQL fixture.
- Scenarios: `quote_read` (constant-arrival-rate) and `order_write`
  (ramping-arrival-rate), with representative JSON payloads.
- Thresholds encode the SLOs below so k6 itself fails the run on breach.
- `SMOKE=1` env var reduces duration/VUs to a few seconds for CI.

### 3. SLOs — `docs/swap-slo.md`

Documented with hardware assumptions, e.g.:

| Metric | Target | Assumption |
|---|---|---|
| Quote p50 | ≤ 25 ms | 4 vCPU / 8 GB app node, Postgres 14 on same host, warm cache |
| Quote p99 | ≤ 120 ms | same |
| Order throughput | ≥ 200 orders/s sustained for 5 min, error rate < 0.1 % | same |

Numbers must be re-derived from a real baseline run, not copied blindly.

### 4. Regression gate — `scripts/check-bench-regression.sh` + nightly job

- Store baseline as `benches/baselines/swap_engine.json` (criterion
  `--message-format json` output, or `critcmp`-style export).
- Script compares current run to baseline and exits non-zero when any
  benchmark's mean regresses by more than `BENCH_REGRESSION_PCT`
  (default 10 %).
- New job in `.github/workflows/rust.yml` (or a dedicated
  `benchmarks.yml`) on `schedule:` (nightly) and `workflow_dispatch`, not on
  every PR, to keep PR latency low.

### 5. Reproducibility and smoke test

- `docs/swap-performance-benchmarks.md` (this file) gains a "Running
  locally" section: exact commands, required services, and how to refresh
  the baseline.
- `scripts/test-bench-regression.sh` runs the comparison script against
  fixtures (a synthetic baseline + a synthetic regressed run), mirroring
  `scripts/test-migration-safety.sh`, so the gate's own logic is covered
  without running real benchmarks in CI.

## Files to change

- `benches/swap_engine.rs` (new)
- `Cargo.toml` (`[[bench]]` entry)
- `loadtest/swap.js` (new)
- `benches/baselines/swap_engine.json` (new, generated)
- `scripts/check-bench-regression.sh` (new)
- `scripts/test-bench-regression.sh` (new)
- `scripts/fixtures/bench-regression/` (new)
- `.github/workflows/rust.yml` or `.github/workflows/benchmarks.yml` (new job)
- `docs/swap-slo.md` (new)
- this file (update with "Running locally")

## Steps in order

1. Land the swap engine hot paths (quote aggregation, routing, fee
   computation, order persistence) and their REST endpoints.
2. Add `benches/swap_engine.rs` with the four benchmarks; run locally to
   produce the first baseline.
3. Add `loadtest/swap.js` with SLO thresholds and a `SMOKE=1` mode.
4. Write `docs/swap-slo.md` from measured numbers.
5. Add `scripts/check-bench-regression.sh` + fixture test.
6. Wire the nightly job and commit the baseline.

## Done when

- `cargo bench --bench swap_engine` runs on a clean checkout and produces all
  four benchmark ids.
- `k6 run loadtest/swap.js` (and `SMOKE=1 k6 run ...`) passes against a local
  stack.
- The nightly job fails when a benchmark regresses past the configured
  percentage, and `scripts/test-bench-regression.sh` proves that logic.
- SLOs are documented with hardware assumptions and match the committed
  baseline.
