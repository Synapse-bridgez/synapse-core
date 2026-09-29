# Tokio Task Leak Detection

A leaked task is one that never completes: a send loop whose sibling was
never aborted, or a receiver on a channel that is never closed. It doesn't
fail. It just parks, and a long-running instance slowly builds up memory and
file descriptors over days. `src/telemetry/task_leak.rs` makes that growth
visible and alertable. Fixing any leak it finds is follow-up work.

## Tagging tasks by origin

Long-lived tasks are started with `task_leak::spawn_tracked(category, fut)`
instead of `tokio::spawn`. A drop guard keeps a live count per category that
stays correct whether the task finishes, panics or is aborted.

| Category | Spawned by | Load that explains it | Tasks per unit of load |
|---|---|---|---:|
| `ws_connection` | `handlers::ws` recv + send tasks | open WebSocket connections (`track_load` guard per connection) | 2 |
| `scheduler_job` | `services::scheduler` job loops | registered jobs (`set_load` in `JobScheduler::start`) | 1 |

To add a category, add a `TaskCategory` variant with its `tasks_per_load`,
then spawn through `spawn_tracked` and report its load.

## Detection is based on load, not raw count

Task count rises and falls with traffic, so the raw count is never alerted
on. For each sample the detector computes

```
excess = live_tasks - tasks_per_load × load
```

For a healthy pool this stays flat however busy the service gets. A category
is flagged when, across the detection window:

- the excess grew by at least `TASK_LEAK_MIN_EXCESS_GROWTH` (default 50)
  tasks, **and**
- it went up steadily: in at least `TASK_LEAK_MIN_MONOTONIC_FRACTION`
  (default 80%) of steps it did not decrease. Leaks only ratchet upward,
  while normal churn goes both ways.

Sampling: every `TASK_LEAK_SAMPLE_INTERVAL_SECS` (default 60) over a window
of `TASK_LEAK_WINDOW_SAMPLES` (default 60), with at least
`TASK_LEAK_MIN_SAMPLES` (default 30) before any verdict. A leak episode
raises `TokioTaskLeakSuspected` once, with `category`, `live`, `load` and
`excess_growth` labels. When it clears, that is logged.

The test `detects_deliberately_introduced_leak` runs a connection handler
that "forgets" to abort its send task next to a correct one, under the same
flat load. Only the leaky one is flagged.

## Metrics

| Metric | Kind | Labels |
|---|---|---|
| `tokio_tasks_live` | Gauge | `category` |
| `tokio_tasks_load` | Gauge | `category` |
| `tokio_tasks_spawned_total` | Gauge (monotonic) | `category` |
| `tokio_task_leak_suspected_total` | Counter | `category` |
| `tokio_runtime_alive_tasks` | Gauge | from tokio's runtime metrics, including untagged tasks |

When `tokio_runtime_alive_tasks` grows while the tagged categories stay
flat, the leak is in an untagged spawn site. Tag it, or use the profiling
endpoints (`docs/profiling-endpoints.md`) to find where tasks are parked.

Operator response: see "Tokio Task Leak Suspected" in
[runbook.md](runbook.md#tokio-task-leak-suspected).
