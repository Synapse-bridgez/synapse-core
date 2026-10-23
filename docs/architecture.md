# Synapse Core — Architecture

## Overview

**Synapse Core** is Phase 1 of the Synapse Bridge ecosystem — a fiat-to-crypto bridging platform built on the [Stellar](https://stellar.org) network. This service acts as a **callback processor** for the [Stellar Anchor Platform](https://github.com/stellar/anchor-platform), receiving webhooks when users deposit fiat currency (e.g., USD) via an anchor, persisting the transaction, and preparing it for downstream processing (swap and cross-chain bridging in later phases).

```
┌─────────────────────────────────────────────────────────────────┐
│                     Synapse Bridge Ecosystem                    │
│                                                                 │
│   Phase 1: Fiat Gateway          Phase 2         Phase 3        │
│   ┌──────────────┐          ┌───────────┐   ┌──────────────┐   │
│   │ synapse-core │  ──────► │   Swap    │──►│ Cross-Chain  │   │
│   │  (this repo) │          │  Engine   │   │   Bridge     │   │
│   └──────────────┘          └───────────┘   └──────────────┘   │
└─────────────────────────────────────────────────────────────────┘
```

---

## System Architecture

```
                                       ┌──────────────────────────┐
 ┌──────┐   Deposits Fiat   ┌──────────┤  Stellar Anchor Platform │
 │ User │ ────────────────►  │          └──────────────────────────┘
 └──────┘                    │                     │
                             │    POST /callback/transaction
                             │                     │
                             │                     ▼
                             │          ┌──────────────────┐
                             │          │   synapse-core    │
                             │          │                  │
                             │          │  Handlers        │
                             │          │    ▼             │
                             │          │  Services ─ ─ ─► Stellar Client ─ ─ ┐
                             │          │    ▼             │                   │
                             │          │  DB Queries      │                  │
                             │          └──────┬───────────┘                  │
                             │                 │                              │
                             │           Read/Write                   Verify On-Chain
                             │                 │                              │
                             │                 ▼                              ▼
                             │          ┌──────────────┐         ┌────────────────────┐
                             │          │  PostgreSQL  │         │ Stellar Horizon API│
                             │          └──────────────┘         └────────────────────┘
```

### Request Flow

1. A user initiates a fiat deposit through the Stellar Anchor Platform.
2. The Anchor Platform processes the deposit and sends a webhook callback to `POST /callback/transaction`.
3. **synapse-core** validates the payload, persists the transaction with status `pending`, and returns a `201 Created`.
4. *(Future)* The Transaction Processor updates the status as the transaction moves through the pipeline (`pending` → `processing` → `completed` / `failed`).

---

## Module Breakdown

### `src/main.rs` — Application Entry Point

Sets up the Axum HTTP server, initializes logging via `tracing-subscriber`, creates the database connection pool, runs SQLx migrations, and mounts all routes.

**Key struct:**
```rust
pub struct AppState {
    db: sqlx::PgPool,
}
```
`AppState` is shared across all handlers via Axum's `State` extractor.

---

### `src/config.rs` — Configuration

Reads environment variables using `dotenvy` and exposes them as a typed `Config` struct.

| Variable              | Required | Default                              | Description                      |
|-----------------------|----------|--------------------------------------|----------------------------------|
| `DATABASE_URL`        | ✅       | —                                    | PostgreSQL connection string     |
| `SERVER_PORT`         | ❌       | `3000`                               | HTTP server listen port          |
| `STELLAR_HORIZON_URL` | ✅       | —                                    | Stellar Horizon API base URL     |

---

### `src/error.rs` — Error Handling *(Planned)*

Will define a centralized `AppError` enum using `thiserror`, covering database, validation, and HTTP errors. Implements Axum's `IntoResponse` for automatic JSON error responses with proper status codes.

---

### `src/db/` — Database Layer

| File         | Purpose                                                        |
|--------------|----------------------------------------------------------------|
| `mod.rs`     | Creates a `PgPool` via `PgPoolOptions` (max 5 connections)    |
| `models.rs`  | Defines the `Transaction` struct (derives `FromRow`)           |

**Transaction Model:**

| Field                    | Type               | Description                              |
|--------------------------|--------------------|------------------------------------------|
| `id`                     | `UUID`             | Primary key (auto-generated via `Uuid::new_v4()`) |
| `stellar_account`        | `String`           | Stellar account address (max 56 chars)   |
| `amount`                 | `BigDecimal`       | Deposit amount                           |
| `asset_code`             | `String`           | Asset code (e.g., `USD`, max 12 chars)   |
| `status`                 | `String`           | Transaction status (`pending` / `processing` / `completed` / `failed`) |
| `created_at`             | `DateTime<Utc>`    | Insertion timestamp                       |
| `updated_at`             | `DateTime<Utc>`    | Last update timestamp                     |
| `anchor_transaction_id`  | `Option<String>`   | ID from the Anchor Platform callback     |
| `callback_type`          | `Option<String>`   | `deposit` or `withdrawal`                |
| `callback_status`        | `Option<String>`   | Original status from the callback        |

---

### `src/handlers/` — HTTP Handlers

| File         | Purpose                                                        |
|--------------|----------------------------------------------------------------|
| `mod.rs`     | `/health` endpoint — returns `"OK"` (to be enhanced with JSON + DB check) |
| `webhook.rs` | *(Planned)* `POST /callback/transaction` — receives Anchor Platform callbacks |

---

### `src/services/` — Business Logic *(Planned)*

| File                       | Purpose                                            |
|----------------------------|----------------------------------------------------|
| `mod.rs`                   | Module exports                                     |
| `transaction_processor.rs` | Orchestrates callback processing: validation, persistence, status transitions |

---

### `src/stellar/` — Stellar Integration *(Planned)*

| File        | Purpose                                                         |
|-------------|-----------------------------------------------------------------|
| `mod.rs`    | Module exports                                                  |
| `client.rs` | HTTP client wrapper for the Stellar Horizon API (account lookups, tx verification) |

---

## Database Schema

Defined in `migrations/20250216000000_init.sql`:

```sql
CREATE TABLE IF NOT EXISTS transactions (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    stellar_account       VARCHAR(56)  NOT NULL,
    amount                NUMERIC      NOT NULL,
    asset_code            VARCHAR(12)  NOT NULL,
    status                VARCHAR(20)  NOT NULL DEFAULT 'pending',
    created_at            TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at            TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    anchor_transaction_id VARCHAR(255),
    callback_type         VARCHAR(20),
    callback_status       VARCHAR(20)
);
```

**Indexes:**
- `idx_transactions_status` — on `status` for filtering
- `idx_transactions_stellar_account` — on `stellar_account` for lookups

---

## Transaction Lifecycle

```
                        Callback received
                              │
                              ▼
                        ┌───────────┐
                        │  pending  │
                        └─────┬─────┘
                              │ Processor picks up
                              ▼
                       ┌─────────────┐
                       │ processing  │
                       └──┬───────┬──┘
                          │       │
     On-chain verification│       │ Verification fails
            passes        │       │   or timeout
                          ▼       ▼
                   ┌──────────┐ ┌────────┐
                   │completed │ │ failed │
                   └──────────┘ └────────┘
```

---

## Deployment Architecture

```
 ┌──────────────────────────┐
 │  Stellar Anchor Platform │
 └────────────┬─────────────┘
              │ HTTP POST
              ▼
     ┌─────────────────────────────────┐
     │        Docker Compose           │
     │                                 │
     │   ┌──────────────────────┐      │
     │   │ synapse-core  :3000  │ ─ ─ ─│─ ─ ► Stellar Horizon API
     │   └──────────┬───────────┘      │          (HTTPS)
     │              │ sqlx             │
     │              ▼                  │
     │   ┌──────────────────────┐      │
     │   │ PostgreSQL    :5432  │      │
     │   └──────────────────────┘      │
     │                                 │
     └─────────────────────────────────┘
```

- **synapse-core** is containerized via a multi-stage `Dockerfile` (Rust build → Debian slim runtime).
- **PostgreSQL 14 Alpine** runs as a sidecar in `docker-compose.yml` with a health check.
- Migrations are bundled inside the container at `/app/migrations` and run automatically on startup.

---

## Technology Stack

| Technology         | Version   | Purpose                            |
|--------------------|-----------|------------------------------------|
| Rust               | 2024 ed.  | Systems language                   |
| Axum               | 0.7       | HTTP framework                     |
| SQLx               | 0.7       | Async PostgreSQL driver + migrations |
| Tokio              | 1.x       | Async runtime                      |
| Serde              | 1.x       | Serialization / deserialization    |
| Tracing            | 0.1       | Structured logging                 |
| Anyhow / Thiserror | 1.x       | Error handling                     |
| Chrono             | 0.4       | Date/time types                    |
| UUID               | 1.x       | UUID generation                    |
| PostgreSQL         | 14+       | Primary data store                 |
| Docker             | —         | Containerization                   |

---

## Future Phases

| Phase   | Component            | Description                                           |
|---------|----------------------|-------------------------------------------------------|
| Phase 1 | **synapse-core**     | Fiat gateway callback processor *(current)*           |
| Phase 2 | Swap Engine          | Convert deposited fiat-backed tokens on Stellar DEX   |
| Phase 3 | Cross-Chain Bridge   | Bridge assets from Stellar to other chains (e.g., EVM)|
