# Prometheus Metrics Retention and Downsampling

## Policy

This repository exports metrics through OTLP and does not deploy or own the
production monitoring services. Retention is implemented at the existing
Prometheus storage boundary with Prometheus recording rules and the
Prometheus-native Thanos block sidecar, Querier, and Compactor extension. This
retains the Prometheus query API used by `PROMETHEUS_URL`; it does not move
application instrumentation or clients to another metrics backend.

| Tier | Resolution | Retention | Intended use |
|---|---:|---:|---|
| Recent | Scrape resolution (typically 15s-60s) | 14 days | Incident debugging, short release scorecards, detailed rate and histogram analysis |
| Medium | 5 minutes | 180 days | Monthly capacity trends and intermediate historical investigation |
| Long | 1 hour | 730 days | Multi-quarter capacity planning and long-range reliability trends |

The companion argument files configure each storage component:

- Prometheus local raw retention and fixed TSDB blocks:
   [`observability/prometheus-retention.args`](../observability/prometheus-retention.args)
- Thanos Sidecar block shipping:
   [`observability/thanos-sidecar.args`](../observability/thanos-sidecar.args)
- Thanos Query downsampling:
   [`observability/thanos-query.args`](../observability/thanos-query.args)
- Thanos Compactor downsampling and tier retention:
   [`observability/thanos-compactor.args`](../observability/thanos-compactor.args)

Prometheus recording rules for error ratio, request latency quantiles, DB
pool saturation, and queue depth are in
[`observability/recording-rules.yml`](../observability/recording-rules.yml).
Include the latter from the Prometheus `rule_files` configuration and keep the
rules evaluating before Prometheus ships their TSDB blocks to Thanos Sidecar.

## Deployment Requirements

The existing monitoring operator must:

1. Apply `prometheus-retention.args` and configure Thanos Sidecar with the
   same existing object store. Prometheus's min/max block duration must remain
   two hours for Sidecar shipping.
2. Run exactly one Thanos Compactor per object-store bucket/compaction stream,
   using `observability/thanos-compactor.args`, a persistent compactor working
   directory, and the platform-managed object-store config mounted at
   `/etc/thanos/objstore.yml`. Do not put object-store credentials in this
   repository.
3. Keep downsampling enabled. Thanos creates 5-minute blocks after raw blocks
   are older than 40 hours and hourly blocks after 5-minute blocks are older
   than 10 days. The configured retention windows leave time for both passes
   before older source resolutions are removed.
4. Point `PROMETHEUS_URL` used by long-term clients at Thanos Query (the
   Prometheus-compatible HTTP API), with query auto-downsampling enabled.
   Continue pointing short-range alerting and diagnostics at the established
   endpoint if it has different latency or availability requirements.
5. Alert on Compactor halt/failure and block-upload age. Retention is not
   enforced while compaction/downsampling is halted; do not treat configured
   flags alone as proof that old blocks are being processed.

Downsampling is not itself a promise of smaller total object-store usage: it
creates additional block resolutions before retention removes older tiers.
Capacity-plan object storage for the overlap and compaction working space.

## Consumer Compatibility

### Release reliability scorecard

The default scorecard window is 24 hours, within raw retention, and continues
using the existing 5-minute PromQL queries and hourly statistical blocks.
When `--window-hours` exceeds 14 days, `cargo xtask scorecard` switches to the
recorded error-ratio and latency-quantile series, queries at 1-hour resolution,
and sends Thanos `max_source_resolution=1h`. Its hourly samples are compared
as hourly blocks (`block_size=1`); it does not evaluate a 5-minute `rate()`
over hourly data. Incident alert episodes also use the hourly query step, so
separate firings of the same alert less than one hour apart may be merged in
long-window incident counts. Use the default short window when exact incident
episode counts matter.

The 30-day overlapping raw/hourly parity fixture in
`xtask/src/commands/scorecard/mod.rs` verifies that equivalent hourly means
produce the same known regression verdict and less than 1% median deviation.
Run it with:

```sh
cargo test -p xtask downsampled_hourly_scorecard_matches_raw_trend_verdict
cargo test -p xtask short_scorecard_windows_keep_raw_promql_queries
```

For a deployed stack, also query the recorded series through Thanos Query over
an overlap period with `step=1h` and `max_source_resolution=1h`; compare its
hourly values with the means of raw 5-minute samples for the same period before
reducing raw retention.

The repository check automates that comparison and exits nonzero if hourly
points are missing or mean relative error exceeds 10%:

```sh
PROMETHEUS_URL=https://thanos-query.internal \
PROMETHEUS_BEARER_TOKEN=... \
python3 scripts/validate_prometheus_downsampling.py
```

Its default window ends 12 days ago and spans the preceding 24 hours, inside
Thanos's raw/hourly overlap (hourly blocks start after 10 days; raw retention
is 14 days). Run it after the first hourly blocks have been compacted and
before reducing raw retention.

### Capacity planning

Historical capacity planning should query
`synapse:db_pool_saturation_ratio:avg5m`,
`synapse:pending_queue_depth:avg5m`, and the recorded error/latency series at
the resolution appropriate to the horizon: 5-minute data for weeks-to-months
and hourly data for multi-quarter trends.
Do not run short range functions such as `rate(...[5m])` over hourly samples.
`xtask pool-advisor` currently returns synthetic data rather than querying
Prometheus, and no production capacity-forecasting implementation is present
in this checkout; it is not possible to claim that tool was validated against
these tiers here. The repository's dependency-health scorecard stores its own
5-minute rollups in Postgres with a 100-day retention policy, so it is
independent of Prometheus retention.

## Rollout

Before lowering retention in an existing deployment, first load the recording
rules, verify their series are present, configure Sidecar uploads and the
Compactor, and wait until 5-minute and hourly blocks are queryable. Compare
raw and downsampled outputs over an overlapping period. Only then apply the
14-day raw retention. Preserve an object-store backup or lifecycle rollback
window while validating the first full 40-hour and 10-day downsampling passes.
