# Build Instructions for Synapse Core

This guide provides multiple approaches to build the Synapse Core project.

## Prerequisites

- Rust 1.84+ (stable): https://rustup.rs/
- PostgreSQL 14+ (for running, not required for local builds)

## Build Method 1: With DATABASE_URL (Recommended)

This method requires a running PostgreSQL database.

```bash
# Start PostgreSQL (or use Docker)
docker run --name synapse-postgres \
  -e POSTGRES_USER=synapse \
  -e POSTGRES_PASSWORD=synapse \
  -e POSTGRES_DB=synapse \
  -p 5432:5432 \
  -d postgres:14-alpine

# Set DATABASE_URL
export DATABASE_URL=postgres://synapse:synapse@localhost:5432/synapse

# Build the project
cargo build
```

## Build Method 2: Using docker-compose

```bash
# Start the database with docker-compose
docker-compose up -d

# Set DATABASE_URL from your .env file
export $(cat .env | grep DATABASE_URL)

# Build the project
cargo build
```

## Build Method 3: Offline Build (sqlx offline mode)

If you don't have a database available, you can use sqlx offline mode:

```bash
# Install sqlx-cli if not already installed
cargo install sqlx-cli

# With a database available, prepare offline data
export DATABASE_URL=postgres://synapse:synapse@localhost:5432/synapse
cargo sqlx prepare

# Now you can build without DATABASE_URL
cargo build  # Works even without DATABASE_URL set now
```

This creates a `.sqlx/` directory with cached query metadata.

## Build Method 4: Skip SQLx Checks (Development Only)

For quick development iteration without a database:

```bash
# Compile without checking database queries
SQLX_OFFLINE=true cargo build
```

**Note**: This may compile but fail at runtime if queries are invalid.

## Quick Setup Script

Use the provided setup script for automated setup:

```bash
./setup.sh
```

This script:
1. Checks for Docker and Rust
2. Starts PostgreSQL container
3. Creates `.env` file
4. Runs migrations

## Verification

After building, verify the project structure:

```bash
cargo check  # Type-check without building
cargo test   # Run unit tests
```

## Troubleshooting

### Error: `set DATABASE_URL to use query macros online`

**Solution 1**: Set DATABASE_URL before building
```bash
export DATABASE_URL=postgres://user:pass@localhost:5432/db
cargo build
```

**Solution 2**: Use offline mode
```bash
cargo sqlx prepare  # With database available
cargo build
```

**Solution 3**: Skip checks temporarily
```bash
SQLX_OFFLINE=true cargo build
```

### Error: Cannot connect to PostgreSQL

Ensure PostgreSQL is running:
```bash
# With Docker
docker ps | grep postgres

# With native PostgreSQL
pg_isready -U synapse
```

### Error: Authentication failed

Check credentials in .env:
```bash
cat .env | grep DATABASE_URL
```

Ensure username, password, host, port, and database name are correct.

## Docker-Compose Alternative

The `docker-compose.yml` in the repo starts PostgreSQL automatically:

```bash
# Start all services
docker-compose up -d

# Run migrations (in the container)
docker-compose exec synapse-core cargo sqlx migrate run

# Stop services
docker-compose down
```
