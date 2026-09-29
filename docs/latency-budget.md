# End-to-End Latency Budget

The webhook → reconciliation pipeline has an end-to-end SLA, split into a
budget for each stage. `src/telemetry/latency_budget.rs` measures how much
of its budget each stage actually uses and alerts when one is consistently
over. This is the stage-level accounting that trace-ID propagation
(`migrations/20260526000000_add_trace_id.sql`) does not provide: trace IDs
say which spans belong together, and this says where the time goes.

## Budgets

| Stage | Default budget | Env override | What it covers |
|---|---:|---|---|
| ingestion | 250 ms | `LATENCY_BUDGET_INGESTION_MS` | `/callback`, `/webhook` handlers (`webhook.*` spans) |
| validation | 50 ms | `LATENCY_BUDGET_VALIDATION_MS` | JSON-schema check (`webhook.validate`) + processor validate stage (`processor.stage.validate`) |
| processing | 30 s | `LATENCY_BUDGET_PROCESSING_MS` | Queue wait while pending + `processor.process_transaction`, minus nested validation |
| settlement | 40 min | `LATENCY_BUDGET_SETTLEMENT_MS` | Wait for the hourly run + the run (`settlement.run`) |
| reconciliation | 12.5 h | `LATENCY_BUDGET_RECONCILIATION_MS` | Wait for the daily run + the run (`reconciliation.run`) |
| **end-to-end SLA** | **sum ≈ 13.2 h** | `LATENCY_BUDGET_E2E_SLA_MS` | Must equal the sum of the stages if set |

If `LATENCY_BUDGET_E2E_SLA_MS` is set and doesn't match the sum of the
stage budgets, the configuration is rejected (logged at startup) and the
defaults are used. That way the per-stage shares can't quietly stop adding
up to the SLA.

The end-to-end figure is dominated by the periodic stages. That is by
design: a transaction is not reconciled until the next daily run.

## How stage latency is derived

No new timers were added. Stage latency comes from the existing `tracing`
spans through a `tracing_subscriber` layer (`StageLatencyLayer`, installed in
`main.rs`):

- A span's time counts toward the stage its name maps to
  (`stage_for_span`).
- Nested spans that belong to a **different** stage are subtracted from the
  enclosing stage, so validation inside processing is not counted twice.
- Nested spans of the **same** stage are covered by the outermost one.
- `pipeline.queue_wait_ms` on a span adds queue time that happened before
  the span started. The processor records how long the transaction sat
  pending.

`attribute_trace` applies the same rules to recorded span data, such as an
exported trace. The unit tests use it on fixture traces with known stage
boundaries.

### Periodic stages

Settlement and reconciliation don't run per transaction. For them, a
transaction's latency is mostly the wait for the next run:

- **Offline** (`attribute_trace`): the stage is charged from the moment the
  transaction became eligible to the end of the first run that started
  after that.
- **Live**: a run can't be linked to individual transactions, so each run
  records the expected contribution per transaction. That is the run time
  plus half the observed interval since the previous run, which is the mean
  wait for uniformly arriving transactions. The first run uses the nominal
  interval (1 h or 24 h).

## Evaluation and alerting

Every `LATENCY_BUDGET_EVAL_INTERVAL_SECS` (default 300), each stage's P95
over the window is compared with its budget. The stage is **consistently**
over budget when at least `LATENCY_BUDGET_BREACH_WINDOWS` (default 4) of its
last `LATENCY_BUDGET_HISTORY_WINDOWS` (default 6) evaluated windows were
over. At that point `PipelineLatencyBudgetExceeded` fires once, with `stage`,
`p95_ms` and `budget_ms` labels and a runbook link. Windows with no samples
don't count, so the daily reconciliation stage is judged on its last few
runs.

This covers reporting and alerting only. Budgets are never rebalanced
automatically.

## Metrics

| Metric | Kind | Labels |
|---|---|---|
| `pipeline_stage_latency_ms` | Histogram | `stage` |
| `pipeline_stage_latency_p95_ms` | Gauge | `stage` |
| `pipeline_stage_budget_ms` | Gauge | `stage` |
| `pipeline_stage_budget_utilization` | Gauge (P95 ÷ budget) | `stage` |

`pipeline_stage_budget_utilization` is the one to put on a dashboard. It
shows which stage is closest to its budget even when none is over.

Operator response: see "Pipeline Latency Budget Exceeded" in
[runbook.md](runbook.md#pipeline-latency-budget-exceeded).
