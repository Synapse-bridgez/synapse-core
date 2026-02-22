# Admin CLI - Quick Reference

## Usage

```bash
synapse-core [COMMAND]
```

## Commands

### Server Management
```bash
# Start the HTTP server (default)
synapse-core serve
synapse-core  # same as serve
```

### Transaction Management
```bash
# Force complete a pending transaction
synapse-core tx force-complete <TX_ID>

# Example
synapse-core tx force-complete 550e8400-e29b-41d4-a716-446655440000
```

### Database Management
```bash
# Run database migrations
synapse-core db migrate
```

### Configuration
```bash
# Validate configuration and display settings
synapse-core config
```

### Help
```bash
# Show help
synapse-core --help
synapse-core tx --help
synapse-core db --help
```

## Development Usage

When running via cargo:
```bash
cargo run -- [COMMAND]
```

Examples:
```bash
cargo run -- serve
cargo run -- tx force-complete 550e8400-e29b-41d4-a716-446655440000
cargo run -- db migrate
cargo run -- config
```

## Production Usage

After building the release binary:
```bash
cargo build --release
./target/release/synapse-core [COMMAND]
```

## Environment Variables

Required:
- `DATABASE_URL` - PostgreSQL connection string
- `STELLAR_HORIZON_URL` - Stellar Horizon endpoint

Optional:
- `SERVER_PORT` - HTTP server port (default: 3000)
- `RUST_LOG` - Logging level (default: info)
