# Cold-Storage Transparent Query Access (#1285)

Audit log rows older than the retention window are archived to cold storage by
the `run_retention` job (`src/db/audit.rs`).  Before this feature, any query
spanning both live and archived time ranges returned incomplete results unless
the caller manually queried both tiers.

This document describes the read path that transparently merges hot and cold
audit log data.

---

## Architecture

```
       ┌─────────────────────────────────────┐
       │         audit_logs_unified (VIEW)   │
       │                                     │
       │   audit_logs (hot)                  │
       │   UNION ALL                         │
       │   cold_audit_logs (cold / rehydrated)│
       └─────────────────────────────────────┘
               ▲
               │  query_audit_logs()
               │  src/db/cold_storage.rs
               │
       GET /admin/audit/unified
```

### Tables

| Table                      | Purpose                                              |
|----------------------------|------------------------------------------------------|
| `audit_logs`               | Live ("hot") audit records — recent, fast I/O       |
| `cold_audit_logs`          | Rehydrated archive rows — physically in Postgres, but sourced from archive files |
| `audit_log_archives`       | Metadata for each archive write (location, checksum) |
| `audit_log_cold_pointers`  | Which archive files have been rehydrated (overlap check) |
| `audit_logs_unified`       | View UNION ALLing both tables, adds `tier` column   |

### Row `tier` field

Every row returned from `audit_logs_unified` has a `tier` field:

- `"hot"` — served from the live `audit_logs` table.
- `"cold"` — served from `cold_audit_logs` (rehydrated from an archive file).

---

## Query Flow

1. **Overlap check** (`check_cold_overlap`): looks for any rows in
   `audit_log_cold_pointers` whose `covers_from`/`covers_to` range overlaps
   the requested time range.  This is cheap (small metadata table).

2. **Unified query** (`query_audit_logs`): issues a single
   `SELECT … FROM audit_logs_unified WHERE timestamp BETWEEN …` — Postgres
   UNION ALLs both tiers transparently.

3. **Result annotation**: `tier_info` in the response describes whether cold
   storage was touched, how many cold rows were in the result, and a latency
   note.

4. **Telemetry**: every query that returns cold-tier rows emits a structured
   `tracing::info!` event with `cold_tier_touched = true` and
   `cold_row_count`.

---

## Rehydration

Cold archive files (gzip NDJSON produced by `run_retention`) must be explicitly
rehydrated before unified queries include their rows.  Rehydration is an
operator-driven step using `rehydrate_archive()` in `src/db/cold_storage.rs`.

If a time-range query overlaps an archive file that has **not** been rehydrated,
the `query_unified_audit_logs_handler` logs a `WARN` with `rehydrated = false`
and the results will be incomplete for that range.  See `warn_cold_data_not_rehydrated`.

---

## Latency Expectations

- **Hot-only queries** (no cold overlap): latency identical to the existing
  `GET /admin/audit/search` endpoint — single-tier read.
- **Cold-tier queries** (cold overlap, data already rehydrated): slightly
  higher due to the UNION ALL and the larger result set from two physical
  tables.  Cold rows are ordinary Postgres table reads once rehydrated; no
  external storage I/O is involved at query time.
- **Un-rehydrated queries**: results are incomplete; no extra latency from
  attempting to read missing cold data.

---

## Admin Endpoints

### `GET /admin/audit/unified`

Query audit logs transparently across hot and cold tiers.

Parameters:
- `from` (required) — RFC 3339 timestamp, inclusive lower bound
- `to` (required) — RFC 3339 timestamp, inclusive upper bound
- `entity_id` (optional) — UUID filter
- `entity_type` (optional) — string filter
- `limit` (optional, default 100, max 1000)

Response:
```json
{
  "total": 1234,
  "data": [ { "id": "...", "tier": "hot", ... }, { "id": "...", "tier": "cold", ... } ],
  "tier_info": {
    "cold_tier_touched": true,
    "cold_row_count": 42,
    "hot_row_count": 958,
    "cold_latency_note": "This query range spans cold-storage-archived data. ..."
  }
}
```

### `GET /admin/audit/cold/pointers`

List all registered cold archive pointers (i.e., which archive files have been
rehydrated and are available for unified queries).

```json
{
  "count": 3,
  "pointers": [
    {
      "id": "...",
      "archive_id": "...",
      "covers_from": "2026-01-01T00:00:00Z",
      "covers_to": "2026-02-01T00:00:00Z",
      "loaded_at": "2026-09-27T12:00:00Z",
      "row_count": 15000
    }
  ]
}
```

---

## Out of Scope

- Changing the cold-storage tiering mechanism (`run_retention`, `ArchiveStorage`
  trait, `LocalDiskArchiveStorage`) — this is the **read path only**.
- Automatic rehydration on first access — rehydration is an explicit operator
  action to avoid uncontrolled I/O from large archive files being loaded on
  demand at query time.
