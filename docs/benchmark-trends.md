# Interpreting the Benchmark Trend Artifact

The `benchmark-trend` CI job (`.github/workflows/rust.yml`) runs
`cargo bench --bench critical_paths` on every push/PR and uploads
`target/criterion/` as a build artifact named
`criterion-benchmark-trend-<commit-sha>`. It does not fail the build —
GitHub-hosted runners are too noisy for hard timing gates; use it as a
trend signal, not a gate. For a hard, pass/fail performance gate see the
system-level load tests in `tests/load/` instead.

## How to read it

1. Download two artifacts (e.g. the current PR's and `main`'s latest) from
   the Actions run summary page.
2. Each benchmark has its own directory under `target/criterion/<name>/`.
   Open `report/index.html` for a chart, or read `new/estimates.json` for
   the raw mean/median/std-dev in nanoseconds.
3. Compare the same benchmark's `estimates.json` across the two artifacts.
   A regression worth investigating is a mean shift larger than the
   reported confidence interval, not any nonzero difference — CI runners
   have enough noise that single-digit percent swings are expected.

## Locally, with baseline comparison

Criterion's built-in baseline comparison is more precise than comparing
artifacts across separate CI runs, since it controls for machine noise:

```bash
cargo bench --bench critical_paths -- --save-baseline before
# make your change
cargo bench --bench critical_paths -- --baseline before
```

The second run prints a `Performance has regressed.` / `improved.` line
per benchmark directly in the terminal.

## Benchmarks covered

See `benches/critical_paths.rs` for the full list, added to as this
project's hot paths grow. Currently covers: callback payload validation,
transaction construction, search WHERE-clause construction, HMAC-SHA256
webhook signing, cursor encode/decode, query-cache key construction, and
the circuit-breaker half-open decision's parse/compare logic.
