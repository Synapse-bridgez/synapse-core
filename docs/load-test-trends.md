# Load Test Trend Dashboard

Complements the point-in-time prose/tables in `docs/load-test-results.md` and
the CI regression gate from issue 49. A run passing that gate can still be
part of a slow, sub-threshold drift across months — this gives that drift
visibility.

## Storage

Every load-test CI run (issue 49's scenarios plus the expanded scenarios from
issue 59) appends one structured JSON record to `tests/load/results/`, named
`<scenario>-<run-timestamp>.json`, e.g.:

```json
{
  "scenario": "mixed_load",
  "timestamp": "2026-08-31T00:00:00Z",
  "git_sha": "abc1234",
  "p50_ms": 42,
  "p99_ms": 310,
  "throughput_rps": 480,
  "error_rate": 0.002
}
```

This directory is the queryable store — plain JSON files, one per run, so no
new database or service is required to start collecting history.

## Trend report

`scripts/generate-load-test-trend-report.sh` reads every record under
`tests/load/results/` for a given scenario and renders a markdown trend table
(p50/p99 latency, throughput, error rate over time) to
`docs/load-test-trend-report.md`. Run it after adding new result records, or
wire it into CI as a scheduled job alongside the issue-49 gate.

## Out of scope

This covers load-test run results only, not live production telemetry —
production performance dashboards are covered by issue 57.
