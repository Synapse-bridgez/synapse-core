# External Synthetic Monitoring

The `External Synthetic Probes` workflow runs every five minutes on a GitHub-hosted
runner, outside the application network. It calls the deployed HTTPS origin, so
checks cover public DNS, TLS termination, ingress/load-balancer routing, and the
application request path. The runner checks:

- `POST /__synthetic/callback`: signed probe authentication, callback schema and
  payload conversion, plus a read-only PostgreSQL `SELECT 1`.
- `POST /__synthetic/graphql`: signed probe authentication and the same GraphQL
  HTTP handler/schema, restricted to `query SyntheticProbe { __typename }`.

The synthetic callback path never inserts a transaction. It therefore creates
no tenant transaction, settlement, reconciliation, or materialized-report row.
After the HMAC middleware authenticates the request, its `SyntheticProbe`
extension bypasses tenant quota accounting. The request remains visible in
logs/traces under the explicit synthetic route/field, but does not contribute to
per-tenant latency metrics.

## Deployment Configuration

Configure these GitHub Actions repository secrets:

- `SYNTHETIC_BASE_URL`: public origin, for example `https://api.example.com`.
- `SYNTHETIC_PROBE_SECRET`: high-entropy HMAC-SHA256 key; set the same value in
  the application environment. Rotate it in both places together.
- `SYNTHETIC_PUSHGATEWAY_URL`: HTTPS URL reachable by GitHub-hosted runners.
- `SYNTHETIC_PUSHGATEWAY_TOKEN`: bearer token required by the authenticated
  Pushgateway ingress.

Expose the Pushgateway to GitHub-hosted runners only through an authenticated
TLS proxy; do not publish an unauthenticated Pushgateway. Prometheus must scrape
the Pushgateway and load the rules in
[`../alerting/prometheus-rules.yml`](../alerting/prometheus-rules.yml). The
existing Alertmanager receiver/routing policy then pages on-call for either a
failing flow (`SyntheticProbeFailed`) or missing/stale reports
(`SyntheticProbeStale`). `probe_type="synthetic"` separates these from
in-cluster health and readiness alerts. A failed run sets its flow's success
gauge to `0`; a successful run restores it to `1` and updates the last-success
timestamp. The runner uses a partial Pushgateway POST on failure so it does not
erase the prior successful timestamp.

Set ingress/IP policy to allow the GitHub-hosted runner egress ranges for these
probe paths, or use a dedicated externally reachable runner. Do not add the
synthetic routes to tenant RUM or tenant report aggregates. Configure the secret
before enabling the schedule; missing application or workflow configuration
is intentionally reported as a probe failure.

For alert response steps, see the **Synthetic Probe Failure** section in
[`runbook.md`](runbook.md#synthetic-probe-failure).