# Contributing to Synapse Core

Thank you for your interest in contributing to Synapse Core! This guide will help you get started with development, understand our coding conventions, and navigate the contribution process.

## Table of Contents

- [Getting Started](#getting-started)
- [Development Setup](#development-setup)
- [Code Style Guide](#code-style-guide)
- [Testing Requirements](#testing-requirements)
- [Pull Request Process](#pull-request-process)
- [Documentation Translations](#documentation-translations)
- [Architecture Decision Records](#architecture-decision-records)
- [Maintainer Rotation & On-Call](#maintainer-rotation--on-call)
- [Code of Conduct](#code-of-conduct)
- [Communication](#communication)

## Getting Started

### Prerequisites

Before you begin, ensure you have the following installed:

| Tool        | Version        | Installation                               |
|-------------|----------------|---------------------------------------------|
| Rust        | 1.84+ (stable) | [rustup.rs](https://rustup.rs/)            |
| PostgreSQL  | 14+            | Via Docker (recommended) or native install |
| Redis       | 7+             | Via Docker or native install               |
| Docker      | 20+            | [docker.com](https://docs.docker.com/get-docker/) |
| sqlx-cli    | 0.7+           | `cargo install sqlx-cli --no-default-features --features postgres` |

### First-Time Setup

1. **Fork and clone the repository**

```bash
git clone https://github.com/YOUR_USERNAME/synapse-core.git
cd synapse-core
```

2. **Set up the development branch**

All contributions must be made against the `develop` branch:

```bash
git checkout develop
git pull origin develop
```

3. **Create your feature branch**

```bash
git checkout -b feat/your-feature-name
```

Branch naming conventions:
- `feat/` - New features
- `fix/` - Bug fixes
- `docs/` - Documentation updates
- `refactor/` - Code refactoring
- `test/` - Test additions or updates
- `chore/` - Maintenance tasks

4. **Set up environment variables**

```bash
cp .env.example .env
```

Edit `.env` with your local configuration:

```env
DATABASE_URL=postgres://synapse:synapse@localhost:5432/synapse
DATABASE_REPLICA_URL=postgres://synapse:synapse@localhost:5433/synapse_replica
REDIS_URL=redis://localhost:6379
SERVER_PORT=3000
STELLAR_HORIZON_URL=https://horizon-testnet.stellar.org
RUST_LOG=debug,synapse_core=trace
```

5. **Start development services**

```bash
docker-compose -f docker-compose.dev.yml up -d
```

This starts:
- PostgreSQL (primary) on port 5432
- PostgreSQL (replica) on port 5433
- Redis on port 6379
- Adminer (database UI) on port 8080

6. **Run database migrations**

```bash
sqlx migrate run
```

7. **Build and run tests**

```bash
cargo build
cargo test
```

8. **Bootstrap the swap engine dev profile (optional)**

If you are working on the swap engine, you can bring up the full stack with
deterministic demo data in a single idempotent command:

```bash
cargo xtask dev up
```

This starts the compose profile, applies migrations, seeds demo tenants,
assets, liquidity provider stubs, sample orders and fills, and prints
ready-to-use endpoints. See
[Swap Engine Local Development Profile](#swap-engine-local-development-profile)
for details.

## Development Setup

### Development Environment

We provide a hot-reload development environment:

```bash
docker-compose -f docker-compose.dev.yml up
```

This uses `cargo-watch` to automatically rebuild when you save files. The first build is slow (~5 minutes), but subsequent rebuilds are fast (~10 seconds).

### Database Management

**Access the database UI:**

Open [http://localhost:8080](http://localhost:8080) in your browser.

Credentials:
- System: `PostgreSQL`
- Server: `postgres`
- Username: `synapse`
- Password: `synapse`
- Database: `synapse`

**Create a test database:**

```bash
docker exec -it synapse-postgres psql -U synapse -c "CREATE DATABASE synapse_test;"
```

**Run migrations manually:**

```bash
sqlx migrate run
```

**Revert last migration:**

```bash
sqlx migrate revert
```

### Code Quality Checks

Before pushing any code, you **must** run and pass all four checks:

```bash
# 1. Format check
cargo fmt --all -- --check

# 2. Lint check
cargo clippy -- -D warnings

# 3. Build check
cargo build

# 4. Test check
cargo test
```

**Auto-fix formatting:**

```bash
cargo fmt --all
```

**Auto-fix some clippy warnings:**

```bash
cargo clippy --fix
```

### Migration Safety

All database migrations must pass the safety checker:

```bash
./scripts/check-migration-safety.sh
```

This ensures migrations are compatible with blue-green deployments. See [docs/migration-safety.md](docs/migration-safety.md) for details.

### Expand/Contract Migrations

Swap engine tables (orders, quotes, fills, events, screenings, submissions)
are migrated using an **expand/contract** practice so that multiple running
replicas never observe a schema they cannot handle. The full policy lives in
[docs/migrations/expand-contract.md](docs/migrations/expand-contract.md); the
short version:

1. **Expand** — add new columns/tables/indexes additively. New columns must be
   nullable or have a default. Ship the migration *before* the code that uses
   it.
2. **Migrate** — deploy application code that writes both old and new shapes
   and backfills data in a separate, batched step.
3. **Contract** — only after all replicas run the new code, drop or rename the
   old shape in a later release.

Rules enforced in CI:

- Dropping or renaming a column still referenced by the current schema is
  rejected.
- Adding a `NOT NULL` column without a default is rejected.
- Long-running locks on high-volume/partitioned tables are flagged; split
  index creation (`CREATE INDEX CONCURRENTLY`) and constraint validation
  (`NOT VALID` + `VALIDATE CONSTRAINT`) into separate migrations.
- Every migration must have a working down path. If a down path genuinely
  cannot exist, record a dated exemption in the policy document and call it
  out in the PR description.

Run the checks locally before pushing:

```bash
cargo xtask migrate lint      # flag unsafe migrations
cargo xtask migrate rehearse  # forward + rollback against a seeded database
```

### Liquidity Provider Adapter Onboarding & Certification

New liquidity provider (LP) adapters must be onboarded through the registration manifest and pass the conformance suite before they can serve live traffic. The full workflow is documented in [docs/lp-adapter-onboarding.md](docs/lp-adapter-onboarding.md); the essentials are:

1. **Declare an adapter manifest** under `src/swap/liquidity/` describing supported assets/pairs, rate limits, precision, and capabilities (streaming quotes, cancellation, partial fills). Manifests are validated at startup.
2. **Load credentials** from environment/secret storage only — never commit secrets. Credential rotation must work without a process restart, and the secrets path must respect `.secrets-allowlist.txt` and pre-commit secret scanning.
3. **Run the conformance suite** against your adapter (see below) and commit the generated certification report.

#### Running the conformance suite locally

The conformance kit lives in `tests/conformance/` and is independent of production wiring, so it can run against the mock adapter in CI or a live sandbox adapter manually:

```bash
# Run the full conformance suite against the mock adapter (as CI does)
cargo test --test conformance

# Run certification for a specific adapter and emit a machine-readable report
cargo xtask certify-adapter --adapter <adapter-name> --output reports/<adapter-name>.json

# Or via the helper script
./scripts/certify-adapter.sh <adapter-name>
```

The suite exercises quote freshness, amount precision, error mapping, timeout behavior, cancellation semantics, and partial-fill reporting. Adapters that declare capabilities they do not honour fail certification rather than degrading silently.

#### Certification requirement

An adapter **must** have a passing certification report before it can be enabled outside test configurations. The registry refuses to activate uncertified adapters in non-test environments. When adding an adapter, include the certification report in your PR and reference it in the description.

### Swap Engine Local Development Profile

The swap engine has a first-class local development profile that is hermetic
(no network access beyond container image pulls) and deterministic (fixed ids
and timestamps) so local state and snapshots are comparable between
contributors.

**One-command bootstrap:**

```bash
cargo xtask dev up
```

This is idempotent — re-running it will not duplicate data. It:

1. Starts the dependencies from `docker-compose.dev.yml` with healthy defaults
   (PostgreSQL primary/replica, Redis, Adminer).
2. Applies all migrations via `sqlx`.
3. Seeds deterministic demo data: tenants, assets, liquidity provider stubs,
   sample swap orders and fills.
4. Prints ready-to-use endpoints (API base URL, Adminer, Redis).

**Teardown / reset:**

```bash
cargo xtask dev seed --reset   # wipe seeded data and re-seed from scratch
cargo xtask dev down           # stop the profile and remove volumes
```

**Safety guard:** seeding and teardown refuse to run against a non-local
database host unless you pass `--force` *and* confirm interactively. The
default profile only ever targets `localhost`. Never point the seed tool at a
shared or production database.

**Environment:** the profile reads `.env.development`. Copy it if you need to
override ports or credentials locally; do not commit secrets.

## Code Style Guide

### Rust Conventions

We follow the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) and enforce them via `rustfmt` and `clippy`.

#### Naming Conventions

```rust
// Types: PascalCase
struct TransactionProcessor {}
enum TransactionStatus {}

// Functions and variables: snake_case
fn process_transaction() {}
let transaction_id = Uuid::new_v4();

// Constants: SCREAMING_SNAKE_CASE
const MAX_RETRY_ATTEMPTS: u32 = 5;

// Lifetimes: short, lowercase
fn process<'a>(data: &'a str) -> &'a str {}
```

#### Module Organization

```rust
// Public exports at the top
pub use self::models::Transaction;
pub use self::queries::*;

// Imports grouped and sorted
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::Config;
use crate::error::AppError;
```

#### Error Handling

We use `thiserror` for custom errors and `anyhow` for application-level error propagation.

**Define custom errors:**

```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("Transaction not found: {0}")]
    TransactionNotFound(Uuid),
    
    #[error("Database error: {0}")]
    DatabaseError(#[from] sqlx::Error),
    
    #[error("Invalid input: {0}")]
    ValidationError(String),
}
```

**Use `?` operator for error propagation:**

```rust
pub async fn get_transaction(pool: &PgPool, id: Uuid) -> Result<Transaction, AppError> {
    let tx = sqlx::query_as!(
        Transaction,
        "SELECT * FROM transactions WHERE id = $1",
        id
    )
    .fetch_optional(pool)
    .await?  // Automatically converts sqlx::Error to AppError
    .ok_or(AppError::TransactionNotFound(id))?;
    
    Ok(tx)
}
```

**Convert to HTTP responses:**

```rust
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            AppError::TransactionNotFound(_) => (StatusCode::NOT_FOUND, self.to_string()),
            AppError::ValidationError(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            AppError::DatabaseError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()),
        };
        
        (status, Json(json!({ "error": message }))).into_response()
    }
}
```

#### Async Patterns

**Use `async/await` consistently:**

```rust
// Good
pub async fn process_transaction(pool: &PgPool, tx: Transaction) -> Result<()> {
    let result = save_transaction(pool, &tx).await?;
    notify_webhook(&tx).await?;
    Ok(())
}

// Avoid blocking operations in async functions
// Bad: std::thread::sleep blocks the executor
// Good: tokio::time::sleep yields to other tasks
tokio::time::sleep(Duration::from_secs(1)).await;
```

**Use `tokio::spawn` for concurrent tasks:**

```rust
let handle1 = tokio::spawn(async move {
    process_batch_1().await
});

let handle2 = tokio::spawn(async move {
    process_batch_2().await
});

let (result1, result2) = tokio::try_join!(handle1, handle2)?;
```

#### Database Queries

**Always use compile-time checked queries:**

```rust
// Good: Compile-time checked with sqlx::query_as!
let tx = sqlx::query_as!(
    Transaction,
    r#"
    SELECT id, amount, status as "status: TransactionStatus"
    FROM transactions
    WHERE id = $1
    "#,
    id
)
.fetch_one(pool)
.await?;

// Avoid: Runtime-checked queries (use only when necessary)
let tx = sqlx::query("SELECT * FROM transactions WHERE id = $1")
    .bind(id)
    .fetch_one(pool)
    .await?;
```

**Always filter by tenant_id for multi-tenant queries:**

```rust
// Good: Enforces tenant isolation
sqlx::query_as!(
    Transaction,
    "SELECT * FROM transactions WHERE id = $1 AND tenant_id = $2",
    transaction_id,
    tenant_id
)
.fetch_optional(pool)
.await?

// Bad: Missing tenant_id filter (security vulnerability)
sqlx::query_as!(
    Transaction,
    "SELECT * FROM transactions WHERE id = $1",
    transaction_id
)
.fetch_optional(pool)
.await?
```

## Testing Requirements

### Test Coverage

All new code must include tests. We aim for:

- **Unit tests** for business logic
- **Integration tests** for API endpoints
- **Property-based tests** for critical paths (using `proptest`)

### Writing Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_process_transaction_success() {
        let pool = setup_test_db().await;
        let tx = create_test_transaction();
        
        let result = process_transaction(&pool, tx).await;
        
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_process_transaction_invalid_amount() {
        let pool = setup_test_db().await;
        let mut tx = create_test_transaction();
        tx.amount = -100;
        
        let result = process_transaction(&pool, tx).await;
        
        assert!(matches!(result, Err(AppError::ValidationError(_))));
    }
}
```

### Running Tests

```bash
# Run all tests
cargo test

# Run specific test
cargo test test_process_transaction

# Run tests with output
cargo test -- --nocapture

# Run tests in a specific module
cargo test --lib transactions::
```

### Test Database

Tests use a separate database. Set it up with:

```bash
export DATABASE_URL=postgres://synapse:synapse@localhost:5432/synapse_test
sqlx migrate run
```

## Pull Request Process

### Before Submitting

1. **Ensure all checks pass:**

```bash
cargo fmt --all -- --check
cargo clippy -- -D warnings
cargo build
cargo test
cargo xtask migrate lint
cargo xtask migrate rehearse
```

If your PR touches swap-related migrations, confirm they follow the
[expand/contract practice](#expandcontract-migrations) and note any dated
exemptions in the PR description.

2. **Update documentation** if you changed public APIs

3. **Add tests** for new functionality

4. **Update CHANGELOG.md** if applicable

5. **Certify new LP adapters:** if your PR adds or changes a liquidity provider adapter, run the conformance suite and attach the certification report (see [Liquidity Provider Adapter Onboarding & Certification](#liquidity-provider-adapter-onboarding--certification))

### PR Guidelines

- **Keep PRs focused:** One feature or fix per PR
- **Write clear descriptions:** Explain what and why, not just how
- **Reference issues:** Use `Fixes #123` or `Closes #123`
- **Respond to feedback:** Address review comments promptly
- **Squash commits:** We use squash-merge, so keep commits clean

### PR Title Format

```
<type>: <description>

Examples:
feat: Add transaction batching endpoint
fix: Resolve race condition in webhook delivery
docs: Update API authentication guide
```

### Review Process

1. **Automated checks** run on every PR (CI, clippy, tests)
2. **At least one maintainer review** is required
3. **Address feedback** and push updates
4. **Maintainer merges** once approved and CI passes

See [Maintainer Rotation & On-Call](#maintainer-rotation--on-call) to find the current reviewer.

## Documentation Translations

Translations of the key onboarding docs (`README.md`, `CONTRIBUTING.md`, `docs/QUICK_START.md`) are welcome. They must be written by a human fluent in the target language; machine-translated content will not be merged.

1. **Open an issue first** (or comment on an existing one) so two people don't translate the same file.
2. **Copy the English source** to `docs/i18n/<lang>/`, using the BCP 47 language tag as the directory name (`es`, `pt-BR`, `zh-Hans`). Keep the file name of the source: `README.md`, `CONTRIBUTING.md`, or `QUICK_START.md`.
3. **Add front matter** at the very top of the file that records what the translation reflects:

   ```markdown
   ---
   source: docs/QUICK_START.md
   source_commit: <full SHA of the last commit that changed the source file>
   ---
   ```

   Get the SHA with `git log -1 --format=%H -- docs/QUICK_START.md`. `source` must be one of the three key docs above, relative to the repo root.
4. **Translate and open a PR** with the `docs` label.

Every PR runs `scripts/check-i18n-staleness.py`. A translation is **stale** when its source file has changed between `source_commit` and `HEAD`; the check posts a warning annotation for each stale file. Stale translations are not deleted, but they must be brought up to date (or the `source_commit` bumped after a review of the source changes) before the next release. See [`docs/i18n/README.md`](docs/i18n/README.md) for the full process and the list of available languages.

## Architecture Decision Records

Significant architectural decisions are documented as ADRs in [`docs/rfcs/`](docs/rfcs/). If your change alters system architecture, add or update an ADR following the [template](docs/rfcs/000-template.md).

## Maintainer Rotation & On-Call

The project uses a version-controlled rotation schedule so contributors always know who is responsible for triage, review, and incident response.

- **Schedule source of truth:** [`docs/governance/rotation.md`](docs/governance/rotation.md) — a structured, human-editable table of `who`, `role`, and `period` (start/end dates).
- **Published view:** the current and upcoming rotation is generated from that schedule and surfaced in the governance docs and the scheduled publish workflow under `.github/workflows/`.
- **Swaps & coverage:** because the schedule is just a file, requesting a swap or coverage is a normal pull request editing the relevant entry. Open a PR titled `chore(rotation): swap <date> <role>` and tag the incoming maintainer.
- **Gaps:** if no maintainer is assigned for the current period, the published view explicitly reports a rotation gap rather than silently falling back to an unspecified default. If you see a gap, open a PR to fill it.

## Code of Conduct

All participants are expected to follow our [Code of Conduct](CODE_OF_CONDUCT.md).
It defines the standards for behavior, and the enforcement workflow for reports:
intake, recusal, confidential review, decision, and communication.

- **Report a concern privately:** email the address in the [Code of Conduct](CODE_OF_CONDUCT.md#1-reporting-intake) with the subject line `CONDUCT:`. Do not raise conduct concerns in public issues, PRs, or Discord.
- **Security issues** go through [SECURITY.md](SECURITY.md), not the conduct channel.

## Communication

- **GitHub Issues:** Bug reports and feature requests
- **GitHub Discussions:** Questions and design discussions
- **Discord:** Real-time chat ([invite link](https://discord.gg/synapse))
- **Weekly sync:** Thursdays at 15:00 UTC

### Getting Help

If you're stuck:

1. Check existing [documentation](docs/)
2. Search [GitHub Issues](https://github.com/synapse-core/synapse-core/issues)
3. Ask in Discord `#dev-help`
4. Tag a maintainer in your PR (see [Maintainer Rotation & On-Call](#maintainer-rotation--on-call))

---

Thank you for contributing to Synapse Core! 🚀
