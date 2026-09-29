# Capacity Forecasting

The daily **Capacity Forecast** workflow queries Prometheus over a rolling
90-day window and publishes Markdown and JSON reports. The Markdown appears in
the workflow run summary; both files are downloadable for 90 days. Forecasts are
reporting only and never trigger scaling actions.

## Forecast model

The `cargo xtask capacity-forecast` command fits ordinary least-squares linear
trends over historical range-query samples. It reports the latest observation,
trend per day, goodness of fit, and estimated time to the configured capacity
threshold. A flat or declining trend does not get an invented exhaustion date;
missing samples, query errors, and unconfigured limits appear explicitly in the
report.

The approximate 95% interval translates the fitted residual band
(plus/minus 1.96 times root-mean-square residual) into earliest/latest threshold
crossing times. It is an explainable noise estimate, not a guarantee: the model
does not account for seasonality, product launches, planned capacity changes,
or correlated/serial residuals. A short history or poor fit should be treated as
low confidence and investigated before acting on a date.

## Metrics and limits

| Dimension | Default Prometheus query | Capacity threshold |
|---|---|---|
| Database pool saturation | `max(db_pool_active_connections / (db_pool_active_connections + db_pool_idle_connections))` | 0.80 of configured pool capacity |
| PostgreSQL storage | `sum(pg_database_size_bytes)` | `DATABASE_STORAGE_CAPACITY_BYTES` |
| Request throughput | `sum(rate(http_request_duration_ms_count[5m]))` | `THROUGHPUT_CAPACITY_RPS`, default 1000 req/s |

The pool query uses the active/idle Prometheus series already referenced by
the pool alert rules. Prometheus must scrape and retain these series. Request
throughput requires `http_request_duration_ms_count` to be exported and
recorded. PostgreSQL storage requires a postgres exporter exposing
`pg_database_size_bytes`. Set the database storage limit to
the usable storage ceiling for the monitored database. The throughput default
uses the 1000 req/s callback load-test target in
[`load-test-results.md`](load-test-results.md); replace it with a measured
sustainable ceiling for the deployed workload.

The default range resolution is one day. Override the metric expressions,
window, resolution, or limits when running manually:

```powershell
$env:PROMETHEUS_URL = "https://prometheus.example"
$env:DATABASE_STORAGE_CAPACITY_BYTES = "107374182400"
$env:THROUGHPUT_CAPACITY_RPS = "800"
cargo xtask capacity-forecast --window-days 90 --step-secs 21600 --out-dir capacity-forecast
```

`PROMETHEUS_BEARER_TOKEN` is optional. The corresponding workflow requires the
`PROMETHEUS_URL` secret and accepts `DATABASE_STORAGE_CAPACITY_BYTES` and
`THROUGHPUT_CAPACITY_RPS` as repository variables. It can also be started
manually from Actions.

To change a query, pass `--pool-query`, `--storage-query`, or
`--throughput-query`. Reports are written to `capacity-forecast/` by default.
For current pool monitoring signals and alert recommendations, see
[`pool_monitoring.md`](pool_monitoring.md).
