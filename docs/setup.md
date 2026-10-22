# Synapse Core — Setup Guide

This guide walks through setting up a local development environment, running the service, and deploying with Docker.

> For an overview of the system design and module responsibilities, see [architecture.md](architecture.md).

---

## Automated Setup (Recommended)

The fastest way to get started is the setup script, which handles prerequisites checking, environment configuration, Docker services, migrations, and a health check in one command:

```bash
./scripts/setup.sh
```

To wipe all existing data and start completely fresh:

```bash
./scripts/setup.sh --reset
```

The script will:
- Verify Rust, Docker, and `psql` are installed
- Copy `.env.example.failover` → `.env` if no `.env` exists
- Start PostgreSQL and Redis via Docker Compose
- Wait for both services to pass their health checks
- Install `sqlx-cli` if needed and run all migrations
- Print next steps to start the server

If you prefer to set things up manually, follow the steps below.

---

## Swap Engine Local Development Profile (One-Command Bootstrap)

For work on the swap engine (settlement, market data, liquidity adapters), use the dedicated dev profile. It is idempotent — running it repeatedly converges to the same state — and hermetic (no external network access beyond container image pulls).

```bash
cargo xtask dev up
```

This single command:

1. Starts the required dependencies via the `dev` compose profile (`docker compose -f docker-compose.dev.yml --profile swap up -d`): PostgreSQL, Redis, and Adminer.
2. Waits for all services to pass their health checks.
3. Applies all migrations from `migrations/` (the same migrations the service runs on boot).
4. Seeds deterministic demo data (see below).
5. Prints ready-to-use endpoints, e.g.:

```
Swap engine dev profile ready.
  API:        http://localhost:3000
  Health:     http://localhost:3000/health
  Adminer:    http://localhost:8080  (postgres / synapse / synapse)
  Redis:      localhost:6379
```

Then start the service with `cargo run` (or `docker compose -f docker-compose.dev.yml up app` for hot reload).

### Deterministic seed data

The seed tool inserts fixed IDs and timestamps so local runs and snapshots are comparable across machines. It seeds, through the same persistence layer the service uses:

- Two demo tenants (`tenant-demo-alpha`, `tenant-demo-beta`)
- Demo assets (XLM, USDC) and liquidity-provider stubs
- A handful of sample swap orders and fills in known states

Seed data is written with `ON CONFLICT DO NOTHING` semantics, so re-running the seed never duplicates rows.

### Teardown and reset

```bash
cargo xtask dev seed            # seed (idempotent)
cargo xtask dev reset           # drop seeded data and re-seed from scratch
cargo xtask dev down            # stop containers (keeps volumes)
cargo xtask dev down --volumes  # stop containers and wipe volumes
```

**Safety guard:** `reset` and `down --volumes` refuse to run against anything that is not a local development database. The tool checks the resolved `DATABASE_URL` host (must be `localhost`, `127.0.0.1`, or a compose service name) and the database name (must end in `_dev` or `_test`). If the check fails, the command aborts with an actionable error and requires an explicit `--force` plus interactive confirmation to proceed. Never point the dev profile at a shared or production database.

### Configuration

The dev profile reads `.env.development` (checked in, contains no real secrets). Override any value with environment variables.

---

## Stellar Settlement Fee Policy (Operators)

Settlement transactions pay a dynamic Stellar inclusion fee derived from Horizon fee statistics, with hard caps to prevent unbounded spend.

### Fee estimation

The fee estimator polls Horizon fee statistics periodically (default every 30s) and derives a recommended inclusion fee (in stroops, carried as `i64` end-to-end) from a configurable percentile of recent ledger fees, clamped to sane bounds.

| Setting                              | Env var                                  | Default | Description                                              |
|--------------------------------------|------------------------------------------|---------|----------------------------------------------------------|
| Fee stats poll interval              | `STELLAR_FEE_POLL_INTERVAL_SECS`         | `30`    | How often Horizon fee stats are refreshed                |
| Fee percentile                       | `STELLAR_FEE_PERCENTILE`                 | `90`    | Percentile of recent inclusion fees to target            |
| Minimum inclusion fee (stroops)      | `STELLAR_FEE_MIN_STROOPS`                | `100`   | Floor applied to the recommended fee                     |
| Maximum inclusion fee (stroops)      | `STELLAR_FEE_MAX_STROOPS`                | `100000`| Ceiling applied to the recommended fee                   |
| Per-tenant fee cap (stroops)         | `STELLAR_FEE_TENANT_CAP_STROOPS`         | `1000000`| Max cumulative fee spend per tenant per settlement window |
| Global fee cap (stroops)             | `STELLAR_FEE_GLOBAL_CAP_STROOPS`         | `10000000`| Max cumulative fee spend across all tenants per window  |
| Stuck-transaction timeout (secs)     | `STELLAR_SETTLEMENT_STUCK_TIMEOUT_SECS`  | `60`    | Time without inclusion before a fee bump is attempted    |
| Max fee bumps per transaction        | `STELLAR_SETTLEMENT_MAX_BUMPS`           | `3`     | Upper bound on bump attempts for a single transaction    |

### Caps and rejection behavior

- Every settlement attempt records the fee actually paid, for cost reporting and reconciliation.
- Before submitting (or bumping), the submission path checks the projected fee against the per-tenant and global caps. If the cap would be exceeded, the transaction is **not** submitted; instead the attempt fails with a classified, actionable error (`FeeCapExceeded`) that names the tenant, the cap, and the projected spend. Operators should treat this as a signal to raise caps or investigate a fee spike — never as a reason to overpay.
- Fee aggregation uses checked arithmetic; precision overflow aborts the attempt with a classified error rather than wrapping.

### Fee bumps for stalled transactions

- If a settlement transaction is not included within `STELLAR_SETTLEMENT_STUCK_TIMEOUT_SECS`, the bump policy attempts a fee-bump (or replacement) transaction per the submission path's rules, up to `STELLAR_SETTLEMENT_MAX_BUMPS` times.
- Before bumping, the policy queries Horizon to confirm the original transaction was not included. If it was, the bump is a **no-op** — an already-included transaction is never double-submitted.
- Transactions whose operations are non-replaceable are not bumped; they are surfaced as failed attempts for operator follow-up.
- If the cap is exhausted mid-bump sequence, the sequence stops and the attempt fails with `FeeCapExceeded`.

### Metrics and alerting

The swap engine emits metrics for fee spikes, bump frequency, and fee-cap rejections. Alert rules live under `monitoring/` and `alerting/`; operators should page on sustained fee-cap rejections and on bump frequency above the configured threshold.

---

## Prerequisites

| Tool        | Version        | Installation                               |
|-------------|----------------|---------------------------------------------|
| Rust        | 1.84+ (stable) | [rustup.rs](https://rustup.rs/)            |
| PostgreSQL  | 14+            | Via Docker (recommended) or native install |
| Docker      | 20+            | [docker.com](https://docs.docker.com/get-docker/) |
| sqlx-cli    | 0.7+           | `cargo install sqlx-cli`                   |

---

## 1. Clone the Repository

```bash
git clone https://github.com/synapse-bridgez/synapse-core.git
cd synapse-core
```

---

## 2. Environment Variables

Copy the example env file and customize:

```bash
cp .env.example .env
```

| Variable              | Required | Default | Description                          |
|-----------------------|----------|---------|--------------------------------------|
| `DATABASE_URL`        | ✅       | —       | PostgreSQL connection string         |
| `SERVER_PORT`         | ❌       | `3000`  | Port for the HTTP server             |
| `STELLAR_HORIZON_URL` | ✅       | —       | Stellar Horizon API endpoint         |

**Example `.env`:**

```env
SERVER_PORT=3000
DATABASE_URL=postgres://synapse:synapse@localhost:5432/synapse
STELLAR_HORIZON_URL=https://horizon-testnet.stellar.org
```

---

## 3. Database Setup

### Option A: Docker (Recommended)

Start a PostgreSQL container:

```bash
docker run --name synapse-postgres \
  -e POSTGRES_USER=synapse \
  -e POSTGRES_PASSWORD=synapse \
  -e POSTGRES_DB=synapse \
  -p 5432:5432 \
  -d postgres:14-alpine
```

Verify it's running:

```bash
docker exec -it synapse-postgres pg_isready -U synapse
```

### Option B: Native PostgreSQL

If you have PostgreSQL installed locally:

```bash
createuser synapse --password  # enter "synapse" when prompted
createdb synapse --owner=synapse
```

Update `DATABASE_URL` in `.env` to match your local credentials.

---

## 4. Run Database Migrations

Migrations run **automatically** when the app starts. To run them manually:

```bash
cargo install sqlx-cli  # if not already installed
sqlx migrate run
```

The migration file `migrations/20250216000000_init.sql` creates the `transactions` table and indexes. See [architecture.md](architecture.md) for the full schema.

---

## 5. Build & Run

### Development

```bash
cargo run
```

You should see output like:

```
INFO  synapse_core > Database migrations completed
INFO  synapse_core > listening on 0.0.0.0:3000
```

### Release Build

```bash
cargo build --release
./target/release/synapse-core
```

### Verify

```bash
curl http://localhost:3000/health
# => OK
```

---

## 6. Running Tests

### Create a Test Database

```bash
docker exec -it synapse-postgres psql -U synapse -c "CREATE DATABASE synapse_test;"
```

### Run Tests

```bash
DATABASE_URL=postgres://synapse:synapse@localhost:5432/synapse_test cargo test
```

> **Note:** Some warnings about unused imports or dead code are expected — they correspond to features planned for future issues.

### Lint & Format

```bash
cargo fmt -- --check   # check formatting
cargo fmt              # auto-format
cargo clippy           # lint
```

---

## 7. Docker Compose (Full Stack)

Spin up both PostgreSQL and the application:

```bash
docker compose up --build
```

This starts:

| Service  | Container           | Port  | Description             |
|----------|---------------------|-------|-------------------------|
| postgres | `synapse-postgres`  | 5432  | PostgreSQL 14 Alpine    |
| app      | `synapse-app`       | 3000  | synapse-core server     |

The app waits for PostgreSQL's health check to pass before starting. Migrations run automatically on boot.

To stop:

```bash
docker compose down
```

To also remove the database volume:

```bash
docker compose down -v
```

---

## 8. Docker Compose (Development — Hot Reload)

The dev compose file mounts source code as volumes and uses `cargo-watch` to rebuild on file changes.

```bash
docker compose -f docker-compose.dev.yml up
```

This starts:

| Service  | Container              | Port  | Description                          |
|----------|------------------------|-------|--------------------------------------|
| postgres | `synapse-postgres-dev` | 5432  | PostgreSQL 14 Alpine                 |
| redis    | `synapse-redis-dev`    | 6379  | Redis 7 Alpine                       |
| adminer  | `synapse-adminer`      | 8080  | Adminer database UI                  |
| app      | `synapse-app-dev`      | 3000  | synapse-core with hot-reload         |

**Hot reload:** Any change to a file under `src/` or `migrations/` triggers an automatic recompile and restart via `cargo-watch`. The first startup is slow (compiling from scratch); subsequent reloads are fast.

**Database UI:** Open [http://localhost:8080](http://localhost:8080) in your browser. Use these credentials:
- System: `PostgreSQL`
- Server: `postgres`
- Username: `synapse`
- Password: `synapse`
- Database: `synapse`

**Debug port:** Port `9229` is exposed for IDE debugger attachment (lldb-server / rust-gdb). Configure your IDE to connect to `localhost:9229`.

**Cargo cache:** The `cargo_registry`, `cargo_git`, and `target_cache` Docker volumes persist the build cache between restarts, so you only pay the full compile cost once.

To stop and remove dev containers:

```bash
docker compose -f docker-compose.dev.yml down
```

To also wipe the build cache volumes (forces a full recompile next time):

```bash
docker compose -f docker-compose.dev.yml down -v
```

---

## 9. Docker (Standalone)

Build the image manually:

```bash
docker build -t synapse-core .
```

Run it (assumes PostgreSQL is reachable at the given URL):

```bash
docker run -p 3000:3000 \
  -e DATABASE_URL=postgres://synapse:synapse@host.docker.internal:5432/synapse \
  -e SERVER_PORT=3000 \
  -e STELLAR_HORIZON_URL=https://horizon-testnet.stellar.org \
  synapse-core
```

> **Note:** Use `host.docker.internal` (macOS/Windows) or `172.17.0.1` (Linux) to reach the host machine's PostgreSQL from inside a container.

---

## 10. Project Structure

```
synapse-core/
├── Cargo.toml               # Dependencies and workspace config
├── .env.example              # Example environment variables
├── docker-compose.yml        # Full-stack Docker setup
├── docker-compose.dev.yml    # Dev profile (hot reload + swap engine deps)
├── .env.development          # Dev profile defaults (no real secrets)
├── xtask/                    # Task runner: `cargo xtask dev up|seed|reset|down`
├── dockerfile                # Multi-stage Rust build
├── migrations/
│   └── 20250216000000_init.sql  # Initial schema migration
├── docs/
│   ├── architecture.md       # System design & module docs
│   └── setup.md              # This file
├── src/
│   ├── main.rs               # Entry point, server setup, migrations
│   ├── config.rs             # Environment variable configuration
│   ├── error.rs              # Custom error types (planned)
│   ├── db/
│   │   ├── mod.rs            # Connection pool creation
│   │   └── models.rs         # Transaction struct & tests
│   ├── handlers/
│   │   ├── mod.rs            # Health check handler
│   │   └── webhook.rs        # Callback handler (planned)
│   ├── services/
│   │   ├── mod.rs            # Service exports (planned)
│   │   └── transaction_processor.rs  # Business logic (planned)
│   └── stellar/
│       ├── mod.rs            # Stellar module exports (planned)
│       └── client.rs         # Horizon API client (planned)
├── tests/
│   └── integration_test.rs   # Integration tests (planned)
└── .github/
    └── workflows/
        └── rust.yml          # CI pipeline
```

---

## API Endpoints

| Method | Path                     | Status       | Description                              |
|--------|--------------------------|--------------|------------------------------------------|
| GET    | `/health`                | ✅ Active    | Health check — returns `"OK"`            |
| POST   | `/callback/transaction`  | 🚧 Planned  | Receive Stellar Anchor Platform webhooks |
| GET    | `/transactions`          | 🚧 Planned  | List transactions with pagination        |
| GET    | `/transactions/:id`      | 🚧 Planned  | Get a single transaction by UUID         |

---

## Troubleshooting

| Problem                                | Solution                                                      |
|----------------------------------------|---------------------------------------------------------------|
| `DATABASE_URL must be set`             | Ensure `.env` exists and `DATABASE_URL` is set                |
| `Failed to connect to test DB`         | Create the test DB: `CREATE DATABASE synapse_test;`           |
| `SQLX_OFFLINE` errors in CI           | Run `cargo sqlx prepare` locally to generate offline query data |
| Port already in use                    | Change `SERVER_PORT` in `.env` or stop the conflicting process |
| Docker connection refused to Postgres  | Use `host.docker.internal` or `172.17.0.1` instead of `localhost` |
| `cargo xtask dev reset` refuses to run | The target DB is not local — check `DATABASE_URL`; use `--force` only for a disposable dev DB |
| Settlement fails with `FeeCapExceeded` | Network fees exceed the tenant/global cap — raise caps or investigate the fee spike |
