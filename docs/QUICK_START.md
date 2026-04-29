# Quick Start Guide for Contributors

This is a condensed guide for experienced developers. For detailed instructions, see [CONTRIBUTING.md](../CONTRIBUTING.md).

## Prerequisites

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install sqlx-cli
cargo install sqlx-cli --no-default-features --features postgres

# Install Docker (for local development)
# See: https://docs.docker.com/get-docker/
```

## Setup (5 minutes)

```bash
# 1. Clone and setup
git clone https://github.com/YOUR_USERNAME/synapse-core.git
cd synapse-core
git checkout develop
git checkout -b feat/your-feature

# 2. Environment
cp .env.example .env
# Edit .env with your settings

# 3. Start services
docker-compose -f docker-compose.dev.yml up -d

# 4. Run migrations
sqlx migrate run

# 5. Build and test
cargo build
cargo test
```

## Development Workflow

```bash
# Make your changes...

# Before committing, run all checks:
cargo fmt --all -- --check  # Format check
cargo clippy -- -D warnings # Lint check
cargo build                 # Build check
cargo test                  # Test check

# If migrations added:
./scripts/check-migration-safety.sh

# Commit and push
git add .
git commit -m "feat: your feature description"
git push origin feat/your-feature
```

## Common Commands

```bash
# Auto-format code
cargo fmt --all

# Run specific test
cargo test test_name

# Run integration tests
cargo test -- --ignored

# Run benchmarks
cargo bench

# Check test coverage
cargo llvm-cov --html
open target/llvm-cov/html/index.html

# Database management
sqlx migrate run           # Apply migrations
sqlx migrate revert        # Revert last migration
sqlx migrate info          # Show migration status

# Docker services
docker-compose -f docker-compose.dev.yml up -d     # Start
docker-compose -f docker-compose.dev.yml down      # Stop
docker-compose -f docker-compose.dev.yml logs -f   # View logs
```

## Project Structure

```
src/
├── main.rs              # Entry point
├── config.rs            # Configuration
├── error.rs             # Error types
├── db/                  # Database layer
│   ├── models.rs        # Data models
│   ├── queries.rs       # Query functions
│   └── pool_manager.rs  # Connection pooling
├── handlers/            # HTTP handlers
├── services/            # Business logic
└── stellar/             # Stellar integration

migrations/              # Database migrations
docs/                    # Documentation
tests/                   # Integration tests
benches/                 # Benchmarks
```

## Code Style Cheat Sheet

```rust
// Error handling
pub async fn get_transaction(pool: &PgPool, id: Uuid) -> Result<Transaction, AppError> {
    let tx = sqlx::query_as!(Transaction, "SELECT * FROM transactions WHERE id = $1", id)
        .fetch_optional(pool)
        .await?
        .ok_or(AppError::TransactionNotFound(id))?;
    Ok(tx)
}

// Logging
use tracing::{info, error, instrument};

#[instrument(skip(pool))]
pub async fn process(pool: &PgPool, id: Uuid) -> Result<()> {
    info!("Processing transaction");
    // ...
}

// Testing
#[tokio::test]
async fn test_transaction_creation() {
    let pool = setup_test_pool().await;
    let tx = create_test_transaction(&pool).await;
    assert_eq!(tx.status, TransactionStatus::Pending);
}

// Multi-tenant queries (ALWAYS filter by tenant_id)
sqlx::query_as!(
    Transaction,
    "SELECT * FROM transactions WHERE id = $1 AND tenant_id = $2",
    transaction_id,
    tenant_id  // REQUIRED
)
```

## PR Checklist

- [ ] Branch from `develop` (not `main`)
- [ ] All 4 checks pass (fmt, clippy, build, test)
- [ ] Tests added/updated
- [ ] Documentation updated
- [ ] Migration safety checked (if applicable)
- [ ] PR description filled out
- [ ] Linked to issue

## Common Issues

**Build fails with OpenSSL error:**
```bash
# Ubuntu/Debian
sudo apt-get install pkg-config libssl-dev

# Fedora/RHEL
sudo dnf install pkg-config openssl-devel

# macOS
brew install openssl
```

**Database connection fails:**
```bash
# Check services are running
docker-compose -f docker-compose.dev.yml ps

# Check DATABASE_URL in .env
echo $DATABASE_URL

# Restart services
docker-compose -f docker-compose.dev.yml restart
```

**Tests fail:**
```bash
# Create test database
docker exec -it synapse-postgres psql -U synapse -c "CREATE DATABASE synapse_test;"

# Set test DATABASE_URL
export DATABASE_URL=postgres://synapse:synapse@localhost:5432/synapse_test
cargo test
```

## Resources

- [Full Contributing Guide](../CONTRIBUTING.md)
- [Architecture Decision Records](adr/README.md)
- [Migration Safety Guide](migration-safety.md)
- [Architecture Overview](architecture.md)
- [Setup Guide](setup.md)

## Getting Help

- **GitHub Issues** - Bug reports and feature requests
- **GitHub Discussions** - Questions and general discussion
- **PR Comments** - Code-specific questions

## Branch Strategy

```
main (production)
  ↑
develop (integration)
  ↑
feat/your-feature (your work)
```

**Always:**
- Branch from `develop`
- PR to `develop`
- Never commit directly to `main` or `develop`

## Commit Message Format

```
<type>: <description>

[optional body]

[optional footer]
```

**Types:**
- `feat:` - New feature
- `fix:` - Bug fix
- `docs:` - Documentation
- `refactor:` - Code refactoring
- `test:` - Test additions/updates
- `chore:` - Maintenance

**Examples:**
```
feat: add transaction retry mechanism
fix: resolve race condition in webhook handler
docs: update API documentation for pagination
refactor: extract validation logic to separate module
test: add integration tests for circuit breaker
chore: update dependencies
```

---

**Ready to contribute?** Pick an issue labeled `good-first-issue` or `phase-1` and get started!
