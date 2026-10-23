# Swap Engine SLOs, Error Budgets, and Alert Routing

SLIs are ratio-of-events over rolling windows, measured at the API edge so a
request (and therefore an order submission) is counted **once**, even when
the engine retries internally. Tenant-caused failures (`4xx`: invalid
requests, insufficient balance, quota/rate limits) are **excluded from the
denominator**; only `5xx` and over-threshold latency count against us.

## SLO catalogue

| SLI | Definition | Source | Window | Objective | Owner |
|---|---|---|---|---|---|
| Request availability (covers quote + order acceptance) | `5xx / (all non-4xx)` | `http_request_duration_ms_count{status}` | 30d | 99.5% good | Platform on-call |
| Request latency (quote latency) | `non-4xx requests slower than 1000ms / all non-4xx` | `http_request_duration_ms_bucket{le="1000"}` | 30d | 95% under 1s | Platform on-call |

**Deferred (no data source yet, so intentionally not defined):** settlement
success rate (`settlement_transactions_total` counts successes only, no failure
series) and lifecycle event delivery timeliness (`webhook_delivery_total` is
declared but not yet emitted). Define these SLIs in the PR that adds the
missing metrics; do not add an SLO before then.

## Error budgets and alert tiers

Budget = 1 − objective (availability 0.5%, latency 5%). Rules are provisioned
as code: SLI ratios in `observability/recording-rules.yml` (group
`synapse-core-slo`), alerts in `alerting/prometheus-rules.yml`
(group `synapse-core-slo`).

| Tier | Burn rate | Windows (both must exceed) | Route |
|---|---|---|---|
| Fast burn | 14.4x (~2% of a 30d budget in 1h) | 1h and 5m | **page** (`severity: critical`, `route: page`) |
| Slow burn | 6x (~5% of budget in 6h) | 6h and 30m | **ticket** (`severity: warning`, `route: ticket`) |

Alertmanager routes on the `route` label: `page` to the paging receiver,
`ticket` to the ticket receiver. Every alert requires
`synapse:slo_http_request_volume:rate1h > 0.1` (about 360 requests/hour) so
idle periods cannot create spurious burn alerts.

## Runbook entries

See `docs/runbook.md`, sections "SLO Burn: Availability Fast/Slow Burn" and
"SLO Burn: Latency Fast/Slow Burn" (meaning, first diagnostic steps,
escalation path). Links are enforced by `scripts/check-alert-runbook-links.py`.

## Budget exhaustion policy

When more than 100% of a 30d error budget is consumed:

1. **Halt rollouts**: no new swap-service deploys or phased-rollout
   percentage increases (rollout plan: `docs/blue-green-deployment.md`) except
   fixes that reduce errors.
2. **Change freeze** on non-reliability changes until the 30d SLI is back
   within objective, or the owner and engineering lead document an exception.
3. A reliability review is opened for the cause of the burn.

At 50% budget consumed, rollouts proceed only with explicit owner sign-off.

## Verification

- `python3 scripts/test_slo_rules.py`: SLI computation against synthetic
  streams, rules reference only defined series/existing metrics, and a dry run.
- Dry run (from the test): a stream with 10% 5xx at 50 rps fires the fast-burn
  condition (threshold 7.2%); a healthy 0.1% stream does not; a single failed
  request on an idle system (0.001 rps) does not page.
- Local stack: `docker-compose.logging.yml` with the rule files mounted.
