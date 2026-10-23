# Contributing to Synapse Core

Thank you for your interest in contributing to Synapse Core! This guide will help you get started with development, understand our coding conventions, and navigate the contribution process.

## Table of Contents

- [Getting Started](#getting-started)
- [Development Setup](#development-setup)
- [Code Style Guide](#code-style-guide)
- [Testing Requirements](#testing-requirements)
- [Pull Request Process](#pull-request-process)
- [Architecture Decision Records](#architecture-decision-records)
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

#### Logging

Use `tracing` for structured logging:

```rust
use tracing::{debug, info, warn, error, instrument};

#[instrument(skip(pool))]
pub async fn process_transaction(pool: &PgPool, tx_id: Uuid) -> Result<()> {
    info!("Processing transaction");
    
    match save_transaction(pool, tx_id).await {
        Ok(_) => {
            debug!(transaction_id = %tx_id, "Transaction saved successfully");
            Ok(())
        }
        Err(e) => {
            error!(error = %e, transaction_id = %tx_id, "Failed to save transaction");
            Err(e)
        }
    }
}
```

**Log levels:**
- `error!` - Errors that require immediate attention
- `warn!` - Potential issues that should be investigated
- `info!` - Important business events (transaction created, webhook received)
- `debug!` - Detailed diagnostic information
- `trace!` - Very verbose debugging (query parameters, response bodies)

#### Documentation

**Document public APIs:**

```rust
/// Processes a transaction and updates its status.
///
/// # Arguments
///
/// * `pool` - Database connection pool
/// * `transaction_id` - UUID of the transaction to process
///
/// # Returns
///
/// Returns `Ok(())` if successful, or an `AppError` if:
/// - Transaction not found
/// - Database connection fails
/// - Stellar verification fails
///
/// # Examples
///
/// ```
/// let result = process_transaction(&pool, transaction_id).await?;
/// ```
pub async fn process_transaction(pool: &PgPool, transaction_id: Uuid) -> Result<(), AppError> {
    // Implementation
}
```

**Add inline comments for complex logic:**

```rust
// Calculate exponential backoff with jitter to prevent thundering herd
let base_delay = Duration::from_secs(2_u64.pow(attempt));
let jitter = rand::random::<u64>() % 1000;
let delay = base_delay + Duration::from_millis(jitter);
```

### Testing Patterns

#### Unit Tests

Place unit tests in the same file as the code:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_transaction_status_transition() {
        let mut tx = Transaction::new();
        assert_eq!(tx.status, TransactionStatus::Pending);
        
        tx.mark_processing();
        assert_eq!(tx.status, TransactionStatus::Processing);
    }
    
    #[tokio::test]
    async fn test_save_transaction() {
        let pool = setup_test_pool().await;
        let tx = Transaction::new();
        
        let result = save_transaction(&pool, &tx).await;
        assert!(result.is_ok());
    }
}
```

#### Integration Tests

Place integration tests in `tests/` directory:

```rust
// tests/transaction_api_test.rs
use synapse_core::*;

#[tokio::test]
async fn test_create_transaction_endpoint() {
    let app = setup_test_app().await;
    
    let response = app
        .post("/api/transactions")
        .header("X-API-Key", "test_key")
        .json(&json!({
            "external_id": "test_001",
            "amount": "100.00",
            "asset_code": "USDC"
        }))
        .send()
        .await;
    
    assert_eq!(response.status(), StatusCode::CREATED);
}
```

#### Property-Based Tests

Use `proptest` for property-based testing:

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn test_amount_always_positive(amount in 0.0..1000000.0f64) {
        let tx = Transaction::new_with_amount(amount);
        assert!(tx.amount > BigDecimal::zero());
    }
}
```

## Testing Requirements

### Test Coverage

We maintain a minimum test coverage of **40%** (enforced in CI) with a target of **60%**.

**Check coverage locally:**

```bash
cargo install cargo-llvm-cov
cargo llvm-cov --html
open target/llvm-cov/html/index.html
```

### Test Categories

1. **Unit Tests** - Test individual functions and modules
   - Run with: `cargo test --lib`
   - Should be fast (<1ms per test)
   - Mock external dependencies

2. **Integration Tests** - Test API endpoints and workflows
   - Run with: `cargo test --test '*'`
   - Use real database (test database)
   - Clean up after each test

3. **Ignored Tests** - Long-running or external dependency tests
   - Run with: `cargo test -- --ignored`
   - Include load tests, external API tests
   - May take several minutes

4. **Benchmarks** - Performance regression tests
   - Run with: `cargo bench`
   - Located in `benches/`
   - Compare against baseline

### Writing Good Tests

**Test naming:**

```rust
#[test]
fn test_<what>_<condition>_<expected_result>() {
    // Example: test_transaction_creation_with_valid_data_succeeds
}
```

**Arrange-Act-Assert pattern:**

```rust
#[tokio::test]
async fn test_transaction_status_update() {
    // Arrange
    let pool = setup_test_pool().await;
    let tx = create_test_transaction(&pool).await;
    
    // Act
    let result = update_transaction_status(&pool, tx.id, TransactionStatus::Completed).await;
    
    // Assert
    assert!(result.is_ok());
    let updated_tx = get_transaction(&pool, tx.id).await.unwrap();
    assert_eq!(updated_tx.status, TransactionStatus::Completed);
}
```

**Clean up test data:**

```rust
#[tokio::test]
async fn test_with_cleanup() {
    let pool = setup_test_pool().await;
    let tx_id = create_test_transaction(&pool).await.id;
    
    // Test logic here
    
    // Cleanup
    sqlx::query!("DELETE FROM transactions WHERE id = $1", tx_id)
        .execute(&pool)
        .await
        .unwrap();
}
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

2. **Check migration safety (if applicable):**

```bash
./scripts/check-migration-safety.sh
```

3. **Update documentation:**
   - Add/update doc comments for public APIs
   - Update relevant docs in `docs/` directory
   - Update CHANGELOG.md (if applicable)

4. **Commit your changes:**

```bash
git add .
git commit -m "feat: add transaction retry mechanism"
```

Commit message format:
- `feat:` - New feature
- `fix:` - Bug fix
- `docs:` - Documentation changes
- `refactor:` - Code refactoring
- `test:` - Test additions/updates
- `chore:` - Maintenance tasks

5. **Push to your fork:**

```bash
git push origin feat/your-feature-name
```

### Creating the Pull Request

1. **Open a PR against `develop` branch** (not `main`)

2. **Fill out the PR template:**

```markdown
## Description
Brief description of what this PR does.

## Related Issue
Closes #123

## Changes Made
- Added transaction retry mechanism
- Updated error handling
- Added integration tests

## Testing
- [ ] Unit tests added/updated
- [ ] Integration tests added/updated
- [ ] Manual testing performed
- [ ] Migration safety checked (if applicable)

## Checklist
- [ ] Code follows style guidelines
- [ ] All tests pass
- [ ] Documentation updated
- [ ] No breaking changes (or documented)
```

3. **Request review** from maintainers

### Review Process

**What reviewers look for:**

1. **Correctness** - Does the code work as intended?
2. **Security** - Are there any security vulnerabilities?
3. **Performance** - Are there any performance concerns?
4. **Maintainability** - Is the code easy to understand and modify?
5. **Testing** - Are there adequate tests?
6. **Documentation** - Is the code well-documented?

**Responding to feedback:**

- Address all comments (or explain why you disagree)
- Push additional commits to the same branch
- Mark conversations as resolved when addressed
- Be respectful and open to suggestions

### Merging

Once approved:

1. Ensure CI passes
2. Squash commits if requested
3. Maintainer will merge to `develop`
4. Delete your feature branch

## Architecture Decision Records

We document significant architectural decisions in ADRs. See [docs/adr/](docs/adr/) for existing records.

**Key ADRs:**

- [ADR-001: Database Partitioning Strategy](docs/adr/001-database-partitioning.md)
- [ADR-002: Circuit Breaker Pattern](docs/adr/002-circuit-breaker.md)
- [ADR-003: Multi-Tenant Isolation](docs/adr/003-multi-tenant-isolation.md)

**When to create an ADR:**

- Choosing between architectural patterns
- Selecting third-party libraries
- Defining system boundaries
- Making security decisions
- Establishing performance targets

**ADR template:** See [docs/adr/000-template.md](docs/adr/000-template.md)

## Communication

### Getting Help

- **GitHub Issues** - Bug reports and feature requests
- **GitHub Discussions** - Questions and general discussion
- **Pull Request Comments** - Code-specific questions

### Reporting Bugs

Use the bug report template and include:

1. **Description** - What happened?
2. **Expected Behavior** - What should have happened?
3. **Steps to Reproduce** - How can we reproduce it?
4. **Environment** - OS, Rust version, etc.
5. **Logs** - Relevant error messages or logs

### Suggesting Features

Use the feature request template and include:

1. **Problem Statement** - What problem does this solve?
2. **Proposed Solution** - How should it work?
3. **Alternatives** - What other approaches did you consider?
4. **Additional Context** - Any other relevant information

## Code of Conduct

We are committed to providing a welcoming and inclusive environment. Please:

- Be respectful and considerate
- Welcome newcomers and help them learn
- Focus on what is best for the community
- Show empathy towards other community members

## License

By contributing, you agree that your contributions will be licensed under the MIT License.

## Additional Resources

- [Rust Book](https://doc.rust-lang.org/book/)
- [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)
- [Axum Documentation](https://docs.rs/axum/)
- [SQLx Documentation](https://docs.rs/sqlx/)
- [Tokio Tutorial](https://tokio.rs/tokio/tutorial)

---

Thank you for contributing to Synapse Core! 🚀
