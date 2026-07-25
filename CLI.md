# Synapse CLI

The synapse-core binary includes command-line tools for managing transactions and database operations.

## Transactions

### List Transactions

List transactions with optional cursor-based pagination and date filtering.

```bash
synapse-core tx list \
  [--cursor <CURSOR>] \
  [--limit <LIMIT>] \
  [--from-date <FROM_DATE>] \
  [--to-date <TO_DATE>] \
  [--format json|table]
```

**Flags:**
- `--cursor <CURSOR>` — Opaque pagination cursor. Always use `next_cursor` from previous response; never construct manually.
- `--limit <LIMIT>` — Max records per page (default: 25, max: 100).
- `--from-date <FROM_DATE>` — Inclusive ISO 8601 start (e.g., `2024-01-01T00:00:00Z`).
- `--to-date <TO_DATE>` — Exclusive ISO 8601 end (e.g., `2024-02-01T00:00:00Z`).
- `--format` — Output format: `json` or `table` (default: `table`).

**Example:**

```bash
# List first 50 transactions since Jan 1, 2024 in JSON
synapse-core tx list --limit 50 --from-date 2024-01-01T00:00:00Z --format json

# Fetch next page using cursor
synapse-core tx list --cursor "eyJwYWdlIjog..." --format table

# Table output
ID                                 Status     Amount       Asset    Created              
---------------------------------- ---------- ------------ -------- -------- ----------
550e8400-e29b-41d4-a716-446655440000 completed  100.00       USD      2024-01-15 10:00:00
```

**Error Handling:**
- Invalid/expired cursor returns 400 error — surface to user and restart pagination from beginning (do not retry).

### Search Transactions

Search transactions by filter, returning a single page of matches.

```bash
synapse-core tx search \
  [--status <STATUS>] \
  [--asset-code <ASSET_CODE>] \
  [--min-amount <MIN_AMOUNT>] \
  [--max-amount <MAX_AMOUNT>] \
  [--from <FROM>] \
  [--to <TO>] \
  [--stellar-account <STELLAR_ACCOUNT>] \
  [--cursor <CURSOR>] \
  [--limit <LIMIT>] \
  [--format json|table]
```

**Filters (all optional):**
- `--status` — Exact transaction status (e.g., `pending`, `completed`).
- `--asset-code` — Exact asset code (e.g., `USD`).
- `--min-amount` — Inclusive minimum amount as decimal (e.g., `10.00`).
- `--max-amount` — Inclusive maximum amount as decimal (e.g., `500.00`).
- `--from` — Inclusive RFC 3339 range start (e.g., `2024-01-01T00:00:00Z`).
- `--to` — Exclusive RFC 3339 range end (e.g., `2024-02-01T00:00:00Z`).
- `--stellar-account` — Exact Stellar account to filter by.
- `--cursor` — Pagination cursor from previous response.
- `--limit` — Max records per page (default: 25, max: 100).
- `--format` — Output format: `json` or `table` (default: `table`).

**Example:**

```bash
# Search for completed USD transactions with amount >= 100
synapse-core tx search --status completed --asset-code USD --min-amount 100.00

# Search by Stellar account
synapse-core tx search --stellar-account GBRPYHIL2CI3WHZDTOOQFC6EB4KJJGUJIIAY3XDBKWV3UYSI7IFYWU4 --format json

# No matches returns empty result, not error
synapse-core tx search --asset-code NONEXISTENT
# Output: Total matches: 0
```

## Database

### Migrate Database

Run all pending database migrations.

```bash
synapse-core db migrate
```

## Backup

### Backup Commands

The following commands are **not yet implemented** and will immediately error with
`"Backup service not yet implemented"`:

```bash
synapse-core backup run [--backup-type hourly|daily|monthly]
synapse-core backup list
synapse-core backup restore <FILENAME>
synapse-core backup cleanup
```

### `backup restore-pitr` — Point-in-time recovery *(implemented)*

Triggers a point-in-time restore via the server's admin API.  This is a
**destructive, data-loss-capable operation**; the live path requires explicit
confirmation.

**Prerequisites:** `ADMIN_API_KEY` environment variable must be set to the same
key the server was started with.

**Syntax:**

```bash
synapse-core backup restore-pitr --timestamp <TIMESTAMP> [--dry-run] [--yes]
```

| Flag | Description |
|------|-------------|
| `--timestamp <TIMESTAMP>` | Target recovery point in ISO 8601 / RFC 3339 format (e.g. `2026-01-15T10:30:00Z`). Required. |
| `--dry-run` | Validate the target timestamp and log the attempt without restoring data. Safe to run without `--yes`. |
| `--yes` | Required for a live (non-dry-run) restore. Absent without `--dry-run`, the command refuses to proceed. |

**Examples:**

```bash
# Validate a target timestamp without touching data
ADMIN_API_KEY=secret synapse-core backup restore-pitr \
  --timestamp 2026-01-15T10:30:00Z --dry-run

# Perform a live restore (requires --yes)
ADMIN_API_KEY=secret synapse-core backup restore-pitr \
  --timestamp 2026-01-15T10:30:00Z --yes
```

The actor is taken from `SYNAPSE_ACTOR`, then `USER`/`LOGNAME`, and recorded in
the server-side audit log alongside the target timestamp.

## Configuration

### Validate Configuration

Validate the current configuration without starting the server.

```bash
synapse-core config
```

## Environment Variables

- `SYNAPSE_API_URL` — Base URL of the Synapse API (default: `http://localhost:3000`).
- `SYNAPSE_API_KEY` — Tenant API key (default: `dev-key`).
- `DATABASE_URL` — Database connection string (required for db commands).
- `STELLAR_HORIZON_URL` — Stellar Horizon URL.
- `REDIS_URL` — Redis connection URL.
- `VAULT_URL` — Vault server URL.
- `VAULT_TOKEN` — Vault token for authentication.
- `ENVIRONMENT` — Environment name (default: `development`).

## Help

Get help for any command:

```bash
synapse-core --help
synapse-core tx --help
synapse-core tx list --help
synapse-core tx search --help
```
