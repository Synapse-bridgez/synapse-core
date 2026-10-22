# Swap tracing and correlation ids

- OTLP export is initialised by `telemetry::init_tracer` (env-configured; degrades gracefully without a collector). Sampling: `SWAP_TRACE_SAMPLE_RATIO` (default 0.01, ratio-based so a trace is sampled consistently).
- `X-Correlation-Id` is accepted (alphanumeric/`-`/`_`, <=128) or generated; invalid/missing never rejects. Use `with_correlation_id` at ingress and `spawn_correlated` for spawned tasks; persist the id on swap orders/events and include it in outbox/webhook payloads.
- Span names: `swap.ingress|quote|route|execute|settle|webhook`. Attributes: `swap.correlation_id`, `swap.tenant_id`, `swap.asset_pair`, `swap.order_id`, `swap.liquidity_provider`. Never put secrets or raw personal data in attributes.
