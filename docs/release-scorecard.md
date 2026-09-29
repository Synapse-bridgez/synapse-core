# Per-Release Reliability Scorecard

Load-test thresholds gate a change *before* merge. The scorecard catches a
different class of regression: one that only shows up under real
production traffic. After each release it compares production reliability
metrics in a window before the deploy with an equal window after it.

```bash
PROMETHEUS_URL=https://prometheus.internal cargo xtask scorecard --release v1.4.0
cargo xtask scorecard --due --lookback-hours 1   # what CI runs hourly
```

Code: `xtask/src/commands/scorecard/`. Automation:
`.github/workflows/release-scorecard.yml`.

## What is compared

| Metric | Source (Prometheus) |
|---|---|
| Error rate (5xx %) | `http_request_duration_ms_count{status=~"5.."}` / all requests |
| Latency P50 / P95 / P99 | `histogram_quantile` over `http_request_duration_ms_bucket` |
| Incidents | Distinct firing episodes of `ALERTS{severity=~"warning\|critical"}`, excluding `Watchdog` |

## Regression vs noise

A metric is flagged as a **regression** only when both of these hold:

1. **It is statistically significant.** Samples (5-minute steps) are first
   averaged into hourly blocks, because consecutive scrapes are
   autocorrelated and would overstate significance. The blocks are then
   compared with a one-sided Mann-Whitney U test at `--alpha` (default
   0.01). Incident counts use an exact conditional binomial test on the two
   rates.
2. **It is big enough to matter.** The block medians must differ by at least
   `--min-relative-change` (default 10%) and by a per-metric absolute floor
   (0.1 percentage points of error rate, 5 / 10 / 20 ms for P50 / P95 / P99).

Other outcomes: *improvement* (significant and large, in the good
direction), *within noise*, or *insufficient data* (fewer than a quarter of
a window's blocks, and at least 3, on either side).

## Releases close together

If B ships soon after A, B's *before* window would contain A's post-release
traffic, and A's *after* window would contain B's code. So:

- B's *before* window starts no earlier than A's deploy, and A's *after*
  window ends at B's deploy.
- Both of a release's windows are then shortened to the **same** length,
  symmetric around the deploy, so time-of-day effects don't bias the
  comparison.
- The report says which neighbouring release clipped a window. If the
  remaining window is too short, the metrics come out as *insufficient
  data* instead of a misleading verdict.

## Automation

The workflow runs hourly and scores every release whose *after* window
closed in the last hour. That is 24 h after the deploy, or the next deploy
if that came sooner. For each scored release it:

- writes `scorecard/<tag>.md` and `.json` (uploaded as an artifact and
  added to the job summary);
- for any regression, opens (or comments on) an issue titled
  "Reliability regression after release `<tag>`" with the
  `release-scorecard` label.

`workflow_dispatch` scores a single tag on demand. Deploy times come from
the tag creation dates. When a deploy lags its tag, pass
`--releases-file deploys.json` (`[{"tag": "...", "deployed_at": "<RFC 3339>"}]`).

This is reporting only. Nothing is rolled back automatically. The scorecard
could become a trigger for automated rollback later, but that is not built
here.

Operator response: see "Release Reliability Regression" in
[runbook.md](runbook.md#release-reliability-regression).
