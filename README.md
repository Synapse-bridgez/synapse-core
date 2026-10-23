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
QUOTE_CACHE_TTL_SECS – TTL for cached swap quotes in seconds (default 10)
QUOTE_CACHE_MAX_STALENESS_SECS – Max age of the price snapshot a cached quote may be served from, in seconds (default 5)
```

Optional market-data (price feed for swap quoting) settings:

```
PRICE_SOURCE_URL – HTTP price source base URL; calls GET {url}/price?base=&quote= (unset disables HTTP ingestion)
PRICE_SOURCE_TIMEOUT_MS – Per-request timeout (default 2000)
PRICE_SOURCE_MAX_RETRIES – Retries after a failed fetch, max 10 (default 2)
PRICE_SOURCE_RETRY_BACKOFF_MS – Base retry backoff, doubled per attempt (default 200)
PRICE_INGESTION_INTERVAL_SECS – Scheduled ingestion interval (default 10)
PRICE_CACHE_TTL_SECS – Fresh snapshot cache TTL in Redis (default 30)
PRICE_MAX_AGE_SECS – Max snapshot age accepted for quoting (default 15)
```

A snapshot is usable for quoting only if it is not flagged `stale` and its source timestamp is within `PRICE_MAX_AGE_SECS`. When the source is unavailable, the last known snapshot is served with `stale = true`.

Swap engine rollout (optional, defaults to off). Set per environment in `.env.development`, `.env.production`, or `.env.example.failover`:

```
SWAP_ENGINE_ENABLED – Process-wide kill switch for swap hand-offs (true/false, default false)
SWAP_ENGINE_TENANT_ALLOWLIST – Comma-separated tenant ids admitted to swap (* = all, empty = none)
```

A tenant also needs the `swap_engine` feature flag enabled. The rollout sequence, kill switch, and stuck-order runbook are in [docs/swap-rollout.md](docs/swap-rollout.md).

**Phase 2 Swap Engine Documentation:**
- [Swap Operator Runbook](docs/swap-operator-runbook.md) — dashboards, alerts, kill switch, stuck orders, escalation
- [Swap Developer Integration Guide](docs/swap-developer-integration-guide.md) — lifecycle, auth, worked example (quote → order), error reference
- [Swap DR Runbook](docs/swap-dr-runbook.md) — backup, restore, and disaster-recovery procedures

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

### ⚡ One-Command Local Development (Swap Engine Profile)

For swap engine work there is a dedicated, idempotent bootstrap that brings up
the full local stack, applies migrations, seeds deterministic demo data, and
prints ready-to-use endpoints:

```bash
cargo xtask dev up
```

This command:

1. Starts the `dev` docker-compose profile (`docker-compose.dev.yml`) with
   PostgreSQL, Redis, and the swap engine dependencies, using the healthy
   defaults from `.env.development`.
2. Applies all `migrations/` via `sqlx` (the same migrations the service runs on
   startup).
3. Seeds deterministic demo data through the service's persistence layer:
   tenants, assets, liquidity provider stubs, sample swap orders, and fills.
   Fixed ids and timestamps are used so local runs and snapshots are
   comparable.
4. Prints the endpoints you can hit immediately, e.g.:

   ```text
   API:        http://localhost:3000
   Health:     http://localhost:3000/health
   Webhook:    http://localhost:3000/webhook
   PostgreSQL: postgres://synapse:synapse@localhost:5432/synapse
   Redis:      redis://localhost:6379
   ```

The bootstrap is safe to re-run: seeding is idempotent and will not duplicate
rows. It requires no external network access beyond container image pulls and
contains no real secrets (local-only credentials from `.env.development`).

#### Seed & Teardown

```bash
# Seed demo data (idempotent)
cargo xtask dev seed

# Tear down seeded demo data and stop the stack
cargo xtask dev down

# Reset: tear down, re-apply migrations, and re-seed from scratch
cargo xtask dev reset
```

`reset`/teardown is guarded: it refuses to run against a non-local database
host (anything other than `localhost`/`127.0.0.1`) unless you explicitly pass
`--force`, and it will prompt for confirmation when the target database does
not look like a development database. This keeps the tooling safe against
accidental use on shared or production databases.

The dev profile is additive and does not change the existing Phase 1 flow:
`docker-compose up -d` and `cargo run` continue to work as documented above.
=======

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

### 🌐 Stellar Testnet Integration Environment

Unit tests and the simulation harness use mocks, so they cannot catch issues that only appear on a real network (sequence handling, trustline/authorization edge cases, ledger-close timing, transaction result codes, Horizon pagination). The end-to-end suite in `tests/integration/` drives a swap order from creation through quote, execution, settlement, and confirmation against real ledger behaviour.

#### 1. Bring up the environment

The suite separates **environment bring-up** from **scenario execution**, so the same scenarios can run against a local container or the hosted testnet.

**Option A – Local Stellar Quickstart (default, used by CI):**

```bash
docker compose -f docker-compose.testnet.yml up -d
# wait for Horizon to report a healthy ledger
curl -sf http://localhost:8000/health || echo "Horizon not ready yet"
```

Tear it down (also removes the ephemeral ledger state):

```bash
docker compose -f docker-compose.testnet.yml down -v
```

**Option B – Hosted Stellar testnet:**

```bash
export STELLAR_TESTNET_MODE=hosted
export STELLAR_HORIZON_URL=https://horizon-testnet.stellar.org
export STELLAR_NETWORK_PASSPHRASE="Test SDF Network ; September 2015"
```

No container is required; accounts are funded via friendbot (with backoff for rate limits). Tests never depend on external asset issuers — each run issues its own assets from a freshly funded account.

#### 2. Run the suite locally

The suite is gated behind an opt-in label so it does not run on every `cargo test`:

```bash
# local quickstart (default)
STELLAR_TESTNET_MODE=local \
DATABASE_URL=postgres://synapse:synapse@localhost:5432/synapse_test \
cargo test --features integration --test integration -- --test-threads=1

# against hosted testnet
STELLAR_TESTNET_MODE=hosted \
STELLAR_HORIZON_URL=https://horizon-testnet.stellar.org \
DATABASE_URL=postgres://synapse:synapse@localhost:5432/synapse_test \
cargo test --features integration --test integration -- --test-threads=1
```

Each test creates unique accounts and assets and cleans up its state, so runs are isolated. The suite asserts on persisted swap records **and** on-chain state, not just HTTP responses.

#### 3. Run in CI

The integration job lives in `.github/workflows/` and runs:

- **Per-PR:** only when the PR carries the `integration-testnet` label (opt-in).
- **Nightly:** on a scheduled run against the hosted testnet.

The job starts the quickstart container, waits for Horizon health, runs the suite with explicit timeouts and bounded ledger polling, then tears the container down. Total runtime is bounded; load/performance testing and chaos injection are out of scope (separate issues).

#### 4. Interpreting failures

- **Environment unreachable:** the suite fails loudly with a self-check error (`testnet environment unreachable at <url>`) instead of skipping silently. Check the container is up (`docker compose -f docker-compose.testnet.yml ps`) or that `STELLAR_HORIZON_URL` is correct.
- **Testnet reset:** if Horizon returns `404` for a previously known account/ledger, the network was reset — re-run against a fresh environment; the failure message names the reset explicitly.
- **Friendbot rate limits:** funding retries with exponential backoff; persistent `429`s surface as a clear failure rather than a hang.
- **Ledger polling timeouts:** waits are bounded; a timeout reports the last observed ledger sequence and the pending transaction hash for diagnosis.

## 📊 Database Partitioning
=======

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

## 🔐 Security Fixes: Tenant Isolation & Auth Hardening

This branch closes an active, unauthenticated cross-tenant data exposure
(`/transactions*`, `/settlements*`, `/ws` resync all returned real,
unscoped data across tenant boundaries), a dead admin quota write path,
missing auth rate limiting on `admin_auth`/`api_key_auth`, and an
unauthenticated, unbounded `/reconnect` session store (removed entirely —
it was never actually connected to the live WebSocket handler).

Root cause of the tenant-isolation gap: every environment this repo
defines connected the app directly as the Postgres `initdb` bootstrap
superuser, which unconditionally bypasses Row-Level Security — so a
correctly-written RLS policy, tenant-context extractor, and scoped-query
helper all existed in the codebase with zero live effect.

Full writeup, evidence, and verification: `docs/postmortem-cross-tenant-leak.md`, `docs/quota-configuration.md`, `docs/auth-rate-limiting.md`.

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

## Handsoff notes

<!-- handsoff-issue-1356 -->
- #1356: [High] Add an audit-log tailing command to the CLI
<!-- handsoff-issue-1345 -->
- #1345: [High] Add SDK request/response logging with secret redaction
<!-- handsoff-issue-1341 -->
- #1341: [High] Build a Go SDK with parity to the Rust SDK
<!-- handsoff-issue-1351 -->
- #1351: [High] Add typed GraphQL codegen for the Rust SDK from the live schema

<!-- handsoff-issue-1479 -->
- #1479: Implement Tenant Self-Service Onboarding and Configuration for Swap Parameters
