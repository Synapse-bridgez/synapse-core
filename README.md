# Synapse Core – Phase 1: Fiat Gateway Callback Processor

Synapse Core is the first component of the Synapse Bridge project. It acts as a **callback processor** for the Stellar Anchor Platform, handling fiat-to-Stellar deposit events. When a user deposits fiat currency (e.g., USD) via an anchor, this service receives a webhook, stores the transaction, and prepares it for the next phases (swap and cross-chain bridging).

This repository is part of the larger Synapse Bridge ecosystem. It is designed to be run alongside the Stellar Anchor Platform (a separate Java‑based service) and a PostgreSQL database.

## 🧱 Project Structure

```
synapse-core/
├── Cargo.toml # Rust dependencies and workspace config
├── .env.example # Example environment variables
├── migrations/ # SQL migrations (sqlx)
│ └── 20250216000000_init.sql
└── src/
├── main.rs # Entry point, server setup, migrations
├── config.rs # Configuration from environment
├── error.rs # (Planned) Custom error types
├── db/ # Database module
│ ├── mod.rs # Connection pool creation
│ └── models.rs # Transaction struct and tests
└── handlers/ # HTTP handlers (e.g. /health, /callback)
└── mod.rs
```

## 🚀 Getting Started

### Prerequisites

- **Rust** (latest stable, 1.84+ recommended) – [Install](https://rustup.rs/)
- **PostgreSQL** 14+ – can be run locally or via Docker
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
DATABASE_URL – PostgreSQL connection string (e.g., postgres://synapse:synapse@localhost:5432/synapse)
SERVER_PORT – Port for the web server (default 3000)
STELLAR_HORIZON_URL – Stellar Horizon endpoint (e.g., https://horizon-testnet.stellar.org)
Start PostgreSQL 3. Using Docker (recommended for development):

```bash
Docker run --name synapse-postgres -e POSTGRES_USER=synapse -e POSTGRES_PASSWORD=synapse -e POSTGRES_DB=synapse -p 5432:5432 -d postgres:14-alpine
Or install PostgreSQL natively and create a database named synapse.
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

#### 📡 Webhook Endpoint (Under Development)

The main purpose of this service is to receive callbacks from the Stellar Anchor Platform. The endpoint will be:

```text
POST /callback/transaction
```

It expects a JSON payload as described in the Anchor Platform callbacks documentation. When implemented, it will store the transaction in the database with status pending.

🤝 Contributing
We welcome contributions! Please see the open issues for tasks labeled phase-1. Each issue includes a description and acceptance criteria.
When contributing:
Fork the repository and create a branch from main.
Write clear, tested code.
Ensure cargo fmt and cargo clippy pass.
Open a pull request with a description of your changes.

📄 License
This project is licensed under the MIT License. See the LICENSE file for details.
