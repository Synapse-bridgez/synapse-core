# Contributing to Synapse Core

Thank you for your interest in contributing to Synapse Core! This guide will help you get started with development, understand our coding conventions, and navigate the contribution process.

## Table of Contents

- [Getting Started](#getting-started)
- [Development Setup](#development-setup)
- [Code Style Guide](#code-style-guide)
- [Testing Requirements](#testing-requirements)
- [Pull Request Process](#pull-request-process)
- [Architecture Decision Records](#architecture-decision-records)
- [Maintainer Rotation & On-Call](#maintainer-rotation--on-call)
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
```

2. **Update documentation** if you changed public APIs

3. **Add tests** for new functionality

4. **Update CHANGELOG.md** if applicable

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

## Architecture Decision Records

Significant architectural decisions are documented as ADRs in [`docs/rfcs/`](docs/rfcs/). If your change alters system architecture, add or update an ADR following the [template](docs/rfcs/000-template.md).

## Maintainer Rotation & On-Call

The project uses a version-controlled rotation schedule so contributors always know who is responsible for triage, review, and incident response.

- **Schedule source of truth:** [`docs/governance/rotation.md`](docs/governance/rotation.md) — a structured, human-editable table of `who`, `role`, and `period` (start/end dates).
- **Published view:** the current and upcoming rotation is generated from that schedule and surfaced in the governance docs and the scheduled publish workflow under `.github/workflows/`.
- **Swaps & coverage:** because the schedule is just a file, requesting a swap or coverage is a normal pull request editing the relevant entry. Open a PR titled `chore(rotation): swap <date> <role>` and tag the incoming maintainer.
- **Gaps:** if no maintainer is assigned for the current period, the published view explicitly reports a rotation gap rather than silently falling back to an unspecified default. If you see a gap, open a PR to fill it.

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
