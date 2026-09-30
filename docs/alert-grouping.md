# Dependency-Based Alert Grouping

The application exports Prometheus/Loki alerts to an externally managed
Alertmanager; this repository does not deploy Alertmanager or own its receivers.
Use `alerting/alertmanager-grouping.yml` as a route fragment and merge its
`group_by`, timing, and nested `routes` fields into the existing top-level
route. Keep the deployment's existing root receiver. Load
`alerting/templates/grouped-alerts.tmpl` from the Alertmanager `templates`
setting and configure the receiver's title/body to use
`{{ template "synapse.group.subject" . }}` and
`{{ template "synapse.group.body" . }}`.

## Grouping Contract

A nonempty `root_cause` label opts an alert into dependency grouping. Those
alerts are grouped by `root_cause`, environment, and cluster. The default route
groups only matching alert name, instance, environment, and dependency labels;
alerts with no root-cause label are never merged with other alert classes just
because they fired at the same time.

`alerting/dependency-alert-groups.json` is the reviewed mapping from known
Prometheus alert names to graph dependencies. Currently
`DatabasePoolUsageHigh`, `DatabasePoolExhausted`, `ReplicationLagHigh`, and
`TransactionInsertMissingPartitionBurst` carry `root_cause=postgres`, matching
the `service -> postgres` edge in
`src/health.rs`. Generic `HighErrorRate`, all Loki alerts, and alerts for other
components remain unclassified until a reliable dependency relationship is
known. Do not assign a root-cause label to an alert based only on temporal
correlation.

Grouping does not inhibit or resolve any alert. Alertmanager keeps each alert
in the notification group's `.Alerts.Firing` or `.Alerts.Resolved` collection.
The supplied template lists every grouped alert with its name, labels,
annotations, firing time, and resolution time. This preserves the individual
symptoms for notification triage and post-incident review.

## Validation

Run the standard-library fixtures and graph-label check with:

```sh
python3 -m unittest -v scripts/test_alert_grouping.py
```

The tests simulate a Postgres incident whose known alerts share one
notification group, and same-time Redis and generic alerts that remain in
separate groups. CI runs this alongside the other alert-rule checks.
