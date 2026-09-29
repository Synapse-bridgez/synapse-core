# Cloud infrastructure cost monitoring

## Goal

Track infrastructure spend over time by component so cost regressions are visible before they become a billing surprise. The dashboard focuses on the same four cost families that matter to this workload: compute, database, cache, and storage.

## Data pipeline

The repo includes a scheduled ingestion helper at `scripts/cloud_cost_ingest.py`. It accepts a billing export from the cloud provider, normalizes the rows, and emits Prometheus-compatible metrics for the existing observability stack.

Example cron entry:

```bash
0 */6 * * * /usr/local/bin/python3 /repo/scripts/cloud_cost_ingest.py \
  --input /var/lib/billing/usage.csv \
  --output /tmp/cloud_cost.prom > /tmp/cloud_cost.prom.log
```

The exporter emits:

- `cloud_cost_total{component="database",category="compute"}`
- `cloud_cost_total{component="cache",category="storage"}`
- `cloud_cost_total{component="compute",category="compute"}`
- `cloud_cost_total{component="storage",category="storage"}`

The dashboard definition in `monitoring/cost-dashboard.json` visualizes those metrics over time and breaks spend by component.

## Dashboard configuration

The dashboard is designed for Grafana and uses the same Prometheus datasource as the app metrics. It includes panels for:

- total spend by component
- compute cost trend
- database spend trend
- cache spend trend
- storage spend trend
- spend variance vs previous day/week

## Operational use

- Flag a regression when any component exceeds its trailing 7-day baseline by more than 20%.
- Use the component split to connect spend spikes to changes in replica count, job churn, or archival tier selection.
- Keep dashboard definitions in version control so cost dashboards move with the same review process as application changes.

## Out of scope

This issue does not include automated optimization or right-sizing actions. It is for visibility only.
