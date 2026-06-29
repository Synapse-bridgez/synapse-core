# Synapse Core – Phase 1: Fiat Gateway Callback Processor

[![codecov](https://codecov.io/gh/Synapse-bridgez/synapse-core/branch/develop/graph/badge.svg)](https://codecov.io/gh/Synapse-bridgez/synapse-core)

Synapse Core is the first component of the Synapse Bridge project. It acts as a **callback processor** for the Stellar Anchor Platform, handling fiat-to-Stellar deposit events. When a user deposits fiat currency (e.g., USD) via an anchor, this service receives a webhook, stores the transaction, and prepares it for the next phases (swap and cross-chain bridging).

This repository is part of the larger Synapse Bridge ecosystem. It is designed to be run alongside the Stellar Anchor Platform and a PostgreSQL database.

## 🧱 Project Structure

```
synapse-core/
├── Cargo.toml # Rust dependencies and workspace config
├── .env.example # Example environment variables
├── migrations/ # SQL migrations (sqlx)
│ ├── 20250216000000_init.sql
│ ├── 20250217000000_partition_transactions.sql
│ └── ... (other migrations)
├── docs/ # Documentation
│ ├── partitioning.md # Database partitioning guide
│ └── partition_utils.sql # Manual partition management utilities
└── src/
├── main.rs # Entry point, server setup, migrations
├── config.rs # Configuration from environment
├── error.rs # (Planned) Custom error types
├── db/ # Database module
│ ├── mod.rs # Connection pool creation
│ ├── models.rs # Transaction struct and tests
│ ├── queries.rs # Database queries
│ └── partition.rs # Partition management
└── handlers/ # HTTP handlers (e.g. /health, /callback)
└── mod.rs
```

## 🚀 Getting Started

### Prerequisites

- **Rust** (latest stable, 1.84+ recommended) – [Install](https://rustup.rs/)
- **PostgreSQL** 14+ – can be run locally or via Docker (required for native partitioning)
- **Stellar Anchor Platform** (optional for development) – see [anchor platform docs](https://github.com/stellar/anchor-platform)

### Setup

1. **Clone the repository**
   ```bash
   git clone https://github.com/synapse-bridgez/synapse-core.git
   cd synapse-core
   ```
2. Set up environment variables

```bash
cp .env.example .env
```

The required variables are:

```
DATABASE_URL – PostgresSQL connection string (e.g., postgres://synapse:synapse@localhost:5432/synapse)
SERVER_PORT – Port for the web server (default 3000)
STELLAR_HORIZON_URL – Stellar Horizon endpoint (e.g., https://horizon-testnet.stellar.org)
REDIS_URL – Redis connection string (e.g., redis://localhost:6379)
```

3. Start PostgresSQL Using Docker (recommended for development):

```bash
Docker run --name synapse-postgres -e POSTGRES_USER=synapse -e POSTGRES_PASSWORD=synapse -e POSTGRES_DB=synapse -p 5432:5432 -d postgres:14-alpine
```

Or install PostgreSQL natively and create a database named synapse.

Alternatively, use docker-compose to start all services (PostgreSQL + Redis):

```bash
docker-compose up -d
```

4. Run database migrations
   The app will automatically run migrations on startup, but you can also run them manually with sqlx:

```bash
cargo install sqlx-cli
DATABASE_URL=postgres://synapse:synapse@localhost:5432/synapse sqlx migrate run
```

5. Build and run the service

```bash
cargo run
```

You should see logs indicating the server started and migrations completed.

### Testing

Create a test database

```bash
docker exec -it synapse-postgres psql -U synapse -c "CREATE DATABASE synapse_test;"
```

Run tests

```bash
DATABASE_URL=postgres://synapse:synapse@localhost:5432/synapse_test cargo test
```

NOTE: Some warnings about unused imports or dead code are expected – they correspond to features planned for future issues.

## 📊 Database Partitioning

The `transactions` table uses time-based partitioning for high-volume scaling:

- **Monthly partitions** by `created_at` timestamp
- **Automatic partition creation** for upcoming months
- **Retention policy** detaches partitions older than 12 months
- **Background maintenance** runs every 24 hours

See [docs/partitioning.md](docs/partitioning.md) for detailed documentation.

### Manual Partition Operations

```sql
-- Create next month's partition
SELECT create_monthly_partition();

-- Detach old partitions (12 months retention)
SELECT detach_old_partitions(12);

-- Run full maintenance
SELECT maintain_partitions();
```

More utilities available in `migrations/partition_utils.sql`.

## 🖥️ CLI Reference

The `synapse-core` binary doubles as a CLI tool.  Run with no arguments (or `serve`) to start the server; use the sub-commands below to inspect a running instance or manage data.

```
synapse-core [COMMAND]

Commands:
  serve       Start the HTTP server (default)
  tx          Transaction management
  db          Database management
  backup      Backup management
  config      Validate configuration
  stats       Query live transaction statistics
  graphql     Execute a GraphQL query or mutation
```

### `stats` — Live Transaction Statistics

All `stats` sub-commands call the running server (default `http://localhost:3000`).  Override with `--url` or the `SYNAPSE_URL` environment variable.  Add `--json` to any command to get machine-readable output.

#### `stats status` — Transaction counts by status

```bash
synapse-core stats status
```

Sample output:

```
STATUS                    COUNT
--------------------------------
completed                   142
failed                        3
pending                      12
processing                    5
```

With `--json`:

```bash
synapse-core stats status --json
```

```json
[
  { "status": "completed", "count": 142 },
  { "status": "failed",    "count": 3   },
  { "status": "pending",   "count": 12  },
  { "status": "processing","count": 5   }
]
```

#### `stats daily` — Daily totals (rolling window)

```bash
# Last 7 days (default)
synapse-core stats daily

# Last 30 days
synapse-core stats daily --days 30
```

Sample output (`--days 7`):

```
DATE         TOTAL AMOUNT    TX COUNT
------------------------------------------
2026-06-23      15420.00           23
2026-06-24      18902.50           31
2026-06-25      12340.75           19
2026-06-26      21000.00           28
2026-06-27       9875.25           14
2026-06-28      17654.00           25
2026-06-29      13210.50           18
```

With `--json`:

```bash
synapse-core stats daily --days 3 --json
```

```json
[
  { "date": "2026-06-27", "total_amount": "9875.25",  "tx_count": 14 },
  { "date": "2026-06-28", "total_amount": "17654.00", "tx_count": 25 },
  { "date": "2026-06-29", "total_amount": "13210.50", "tx_count": 18 }
]
```

Valid range for `--days`: **1–365**.  The server rejects values outside this range with HTTP 400.

#### `stats assets` — Per-asset volume

```bash
synapse-core stats assets
```

Sample output:

```
ASSET         TOTAL AMOUNT    TX COUNT         AVG AMOUNT
------------------------------------------------------------
USDC          108402.25            87           1245.43
XLM            52100.00            45           1157.78
```

With `--json`:

```bash
synapse-core stats assets --json
```

```json
[
  {
    "asset_code":    "USDC",
    "total_amount":  "108402.25",
    "tx_count":      87,
    "avg_amount":    "1245.43"
  },
  {
    "asset_code":    "XLM",
    "total_amount":  "52100.00",
    "tx_count":      45,
    "avg_amount":    "1157.78"
  }
]
```

#### `stats cache` — Query-cache and idempotency-cache metrics

```bash
synapse-core stats cache
```

Sample output:

```
Query cache:
  hits:     312
  misses:   48
  hit_rate: 86.67%

Idempotency cache:
  cache_hits:      198
  cache_misses:    22
  lock_acquired:   210
  lock_contention: 4
  errors:          0
  fallback_count:  2
```

With `--json`:

```bash
synapse-core stats cache --json
```

```json
{
  "query_cache": {
    "hits": 312, "misses": 48, "total": 360,
    "hit_rate": 0.8667,
    "memory_hits": 280, "memory_misses": 32,
    "memory_total": 312, "memory_hit_rate": 0.8974
  },
  "idempotency_cache_hits":      198,
  "idempotency_cache_misses":    22,
  "idempotency_lock_acquired":   210,
  "idempotency_lock_contention": 4,
  "idempotency_errors":          0,
  "idempotency_fallback_count":  2
}
```

---

### `graphql query` — Execute GraphQL queries and mutations

Sends a GraphQL document to `POST /graphql` and pretty-prints the response.

```
synapse-core graphql query <GRAPHQL_QUERY> [OPTIONS]

Arguments:
  <GRAPHQL_QUERY>   GraphQL query or mutation string

Options:
  --variables <JSON>   Optional JSON object for variables
  --url <URL>          Server base URL [default: http://localhost:3000]
                       [env: SYNAPSE_URL]
```

#### List all transactions

```bash
synapse-core graphql query '{ transactions { id status } }'
```

Sample output:

```json
{
  "data": {
    "transactions": [
      { "id": "3f6e2a1b-...", "status": "completed" },
      { "id": "7c4d9f3e-...", "status": "pending"   }
    ]
  }
}
```

#### Look up a single transaction

```bash
synapse-core graphql query \
  '{ transaction(id: "3f6e2a1b-1234-5678-abcd-000000000001") { id status amount assetCode } }'
```

Sample output:

```json
{
  "data": {
    "transaction": {
      "id":        "3f6e2a1b-1234-5678-abcd-000000000001",
      "status":    "completed",
      "amount":    "100.50",
      "assetCode": "USDC"
    }
  }
}
```

#### Filter with variables

```bash
synapse-core graphql query \
  '{ transactions { id status } }' \
  --variables '{"filter":{"status":"pending"}}'
```

#### Force-complete a transaction (mutation)

```bash
synapse-core graphql query \
  'mutation { forceCompleteTransaction(id: "3f6e2a1b-1234-5678-abcd-000000000001") { id status } }'
```

Sample output:

```json
{
  "data": {
    "forceCompleteTransaction": {
      "id":     "3f6e2a1b-1234-5678-abcd-000000000001",
      "status": "completed"
    }
  }
}
```

#### GraphQL errors vs transport errors

When the server returns an `errors` array (HTTP 200), the CLI prints the errors to **stderr** and exits non-zero:

```
GraphQL errors:
[
  {
    "message": "Unsupported GraphQL query",
    "locations": [{ "line": 1, "column": 3 }]
  }
]
```

Transport failures (connection refused, DNS errors, etc.) are reported as `Transport error – could not reach <url>: …` and also exit non-zero.

---

#### 📡 Webhook Endpoint (Under Development)

The main purpose of this service is to receive callbacks from the Stellar Anchor Platform. The endpoint will be:

```text
POST /webhook
```

It expects a JSON payload with an `X-Idempotency-Key` header (typically the `anchor_transaction_id`) to prevent duplicate processing. When implemented, it will store the transaction in the database with status pending.

##### Idempotency Protection

Webhooks are protected against duplicate delivery using Redis-based idempotency:
- Each webhook must include an `X-Idempotency-Key` header
- Duplicate requests within 24 hours return cached responses
- Concurrent requests for the same key return `429 Too Many Requests`
- See [docs/idempotency.md](docs/idempotency.md) for detailed documentation

#### 🔄 Circuit Breaker

The Stellar Horizon client includes a circuit breaker to prevent cascading failures:
- Automatically detects when Horizon API is down or slow
- Fails fast instead of waiting for timeouts
- Configurable failure threshold and reset timeout
- See [docs/circuit-breaker.md](docs/circuit-breaker.md) for detailed documentation

## 🤝 Contributing

We welcome contributions! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for detailed guidelines on:

- Development setup and workflow
- Code style and conventions
- Testing requirements
- Pull request process
- Architecture Decision Records (ADRs)

**Quick start for contributors:**

1. Fork the repository and create a branch from `develop`
2. Set up your development environment (see [CONTRIBUTING.md](CONTRIBUTING.md))
3. Write clear, tested code following our style guide
4. Ensure all checks pass: `cargo fmt`, `cargo clippy`, `cargo build`, `cargo test`
5. Open a pull request against `develop` with a clear description

See the [open issues](https://github.com/synapse-bridgez/synapse-core/issues) for tasks labeled `phase-1` or `good-first-issue`.

📄 License
This project is licensed under the MIT License. See the LICENSE file for details.
