# CDC export to the analytics warehouse

- **Capture:** PostgreSQL logical decoding. Publication `synapse_swap_cdc` (migration `20261021000001`), slot `synapse_swap_cdc_slot`, connector config in `deploy/cdc/connector.json`. Initial snapshot, then incremental streaming.
- **Warehouse layers:** `staging` (raw events keyed by `(aggregate_id, version)`) -> `curated` views.
- **Delivery:** at-least-once, ordered per aggregate by `version`. Staging upserts on the key, so replays never double-count (`cdc::staging_key`).
- **Replay:** reset the connector offset (or re-snapshot), re-run; staging upserts make it idempotent. Curated views are rebuilt from staging.
- **Schema evolution:** expand/contract. Additive nullable columns land in staging untouched; curated views select explicit columns, so they stay valid. Drop columns only after views stop referencing them.
- **PII:** `cdc::PII_EXCLUDED_COLUMNS` and connector `column.exclude.list` drop PII before export.
- **Slot/lag:** `cdc::evaluate` flags `lag_high` (`CDC_MAX_LAG_SECS`, default 300), `slot_bloat` (`CDC_MAX_SLOT_RETAINED_BYTES`, default 5GiB) and `slot_missing`; alert on these, and set `max_slot_wal_keep_size` to bound WAL growth.
- **Partitions:** publication uses `publish_via_partition_root`, so detach/retention does not orphan warehouse rows.
