# End-to-end tutorial: CLI + Rust SDK

This walkthrough shows two ways to work with Synapse deposits:

1. **CLI (look back)** — list recent transactions and spot a completed deposit.
2. **Rust SDK (look forward)** — subscribe to real-time status events and react to the next one.

Both tools talk to the same HTTP API (`GET /transactions` and `GET /ws`). The CLI is good for ad-hoc inspection and scripting; the SDK is better when you need a long-lived listener inside your application.

## Prerequisites

- Rust toolchain (see [Quick Start](QUICK_START.md))
- A running Synapse Core server (see [Setup](setup.md))
- Built binaries:

```bash
# API server + embedded CLI
cargo build --bin synapse-core

# Rust SDK examples
cd sdks/rust
cargo build --examples
```

Set credentials (used by both the CLI and SDK):

```bash
export SYNAPSE_API_URL=http://localhost:3000
export SYNAPSE_API_KEY=dev-key
```

> **Note on the mock server:** `cli/synapse-cli` ships a lightweight `mock-server` binary, but it currently stubs only admin reconciliation routes. For this tutorial, run a real `synapse-core` server locally (or point at a dev deployment). Integration tests under `cli/synapse-cli/tests/` use [wiremock](https://github.com/LukeMathWalker/wiremock-rs) for HTTP stubs when you need isolated verification.

---

## Scenario

You operate a fiat on-ramp integration. Deposits arrive as transactions with `callback_type: "deposit"`. You want to:

1. Confirm that a recent deposit landed (`status: "completed"`).
2. Keep a small Rust program running that prints a line and exits when the **next** deposit completes.

---

## Part 1 — Find a recent deposit with the CLI

The `synapse-core` binary includes transaction commands under `tx`. Listing hits `GET /transactions` with cursor pagination (see [API reference](api-reference.md#get-transactions)).

### List the newest transactions

```bash
synapse-core tx list --limit 10 --format json
```

Optional date window (ISO 8601, `from_date` inclusive / `to_date` exclusive — same contract as the handler):

```bash
synapse-core tx list \
  --limit 25 \
  --from-date 2026-06-01T00:00:00Z \
  --to-date 2026-07-01T00:00:00Z \
  --format json
```

### Response shape

The API returns paginated data:

```json
{
  "data": [
    {
      "id": "550e8400-e29b-41d4-a716-446655440000",
      "stellar_account": "GABC1234567890123456789012345678901234567890123456789012",
      "amount": "100.00",
      "asset_code": "USDC",
      "status": "completed",
      "callback_type": "deposit",
      "created_at": "2026-06-27T10:00:00Z",
      "updated_at": "2026-06-27T10:01:00Z"
    }
  ],
  "meta": {
    "next_cursor": "eyJ0cyI6...",
    "has_more": true
  }
}
```

Pick the most recent row where `callback_type` is `"deposit"`. Note its `id` — you will compare against live events in Part 2.

### Table output (default)

Human-readable columns are available without `--format json`:

```bash
synapse-core tx list --limit 5
```

### Pagination

Follow `meta.next_cursor` from the JSON response — never construct cursors manually:

```bash
synapse-core tx list --cursor "<next_cursor from previous response>" --format json
```

An invalid or expired cursor returns HTTP 400; restart from the first page instead of retrying the same cursor.

### CLI vs. standalone `synapse` binary

The standalone `synapse` CLI (`cli/synapse-cli`) exposes `synapse transactions export` and `synapse transactions get` for bulk export and single-record lookup. **Listing** with pagination is provided by `synapse-core tx list`, which calls the same `/transactions` endpoint the SDK uses below.

---

## Part 2 — Subscribe to the next deposit with the Rust SDK

The SDK exposes real-time updates through `SynapseClient::events().subscribe()`. It opens a WebSocket to `GET /ws`, parses [`TransactionStatusUpdate`](https://github.com/Synapse-bridgez/synapse-core/blob/main/src/handlers/ws.rs) payloads, and invokes your callbacks until you return `false` or the connection closes.

See also: [WebSocket pagination](websocket-pagination.md) for the wire protocol and [events_subscribe.rs](../sdks/rust/examples/events_subscribe.rs) for a minimal runnable example.

### Create a watcher program

Add a binary to your crate (or adapt the example):

```toml
# Cargo.toml
[dependencies]
synapse-sdk = { path = "../sdks/rust" }   # adjust path to your checkout
tokio = { version = "1", features = ["full"] }
```

```rust
use synapse_sdk::{SynapseClient, SynapseError};

#[tokio::main]
async fn main() {
    let base_url = std::env::var("SYNAPSE_API_URL")
        .unwrap_or_else(|_| "http://localhost:3000".to_string());
    let api_key = std::env::var("SYNAPSE_API_KEY")
        .unwrap_or_else(|_| "dev-key".to_string());

    let client = SynapseClient::new(base_url, api_key);

    println!("Waiting for the next completed deposit event…");

    let result = client
        .events()
        .subscribe(
            |event| {
                println!(
                    "[{}] tx {} -> {} ({})",
                    event.timestamp,
                    event.transaction_id,
                    event.status,
                    event.message.as_deref().unwrap_or("-"),
                );

                if event.status == "completed" {
                    // React once — e.g. enqueue settlement, notify ops, etc.
                    println!("Reacting to completed deposit {}", event.transaction_id);
                    return false; // close subscription after handling
                }

                true // keep listening for non-terminal updates
            },
            |err| {
                eprintln!("subscription error (continuing): {}", err);
                true
            },
        )
        .await;

    match result {
        Ok(()) => println!("subscription closed"),
        Err(SynapseError::Http { status: 0, body }) => {
            eprintln!("connection failed: {}", body);
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}
```

Run it:

```bash
cd sdks/rust
SYNAPSE_API_URL=http://localhost:3000 SYNAPSE_API_KEY=dev-key \
  cargo run --example events_subscribe
```

The bundled example stops after five events; replace the `count < 5` guard with the `completed` check above for the “next deposit” behaviour.

### WebSocket authentication

Connections require a non-empty `token` query parameter:

```
ws://localhost:3000/ws?token=dev-key
```

The SDK passes this during the handshake when you call `subscribe()`. Any non-empty token up to 1024 characters is accepted by the server validator.

### Event payload

Each message is a JSON object (no `type` wrapper for status updates):

```json
{
  "transaction_id": "550e8400-e29b-41d4-a716-446655440000",
  "tenant_id": "660e8400-e29b-41d4-a716-446655440001",
  "status": "completed",
  "timestamp": "2026-06-27T10:01:00Z",
  "message": "Transaction processed successfully"
}
```

When a deposit callback is processed, the server broadcasts these updates on the internal channel; all connected WebSocket clients receive them.

---

## Part 3 — Tie it together

| Step | Tool | Action |
|------|------|--------|
| 1 | CLI | `synapse-core tx list --limit 10 --format json` — identify a recent `callback_type: "deposit"` row |
| 2 | SDK | Run the subscriber **before** injecting a new test deposit |
| 3 | API | POST a deposit callback (see [webhook handler](webhook-handler.md)) or use your anchor sandbox |
| 4 | SDK | Observer prints the event; program exits after `status == "completed"` |
| 5 | CLI | Re-run `synapse-core tx list` — the new deposit appears in the first page |

This highlights the split:

- **CLI** = snapshot queries, pipes to `jq`, cron-friendly one-shots.
- **SDK** = typed client, pagination helpers (`transactions().list()`), and persistent `events().subscribe()` for reactive workflows.

---

## SDK equivalent of the list step

The CLI list command mirrors `client.transactions().list()`:

```rust
use synapse_sdk::{ListParams, SynapseClient, SynapseError};

#[tokio::main]
async fn main() {
    let client = SynapseClient::new(
        std::env::var("SYNAPSE_API_URL").unwrap_or_else(|_| "http://localhost:3000".into()),
        std::env::var("SYNAPSE_API_KEY").unwrap_or_else(|_| "dev-key".into()),
    );

    let page = client
        .transactions()
        .list(ListParams {
            limit: Some(10),
            ..Default::default()
        })
        .await
        .expect("list request");

    for tx in &page.data {
        if tx.callback_type.as_deref() == Some("deposit") {
            println!("{}  {}  {} {}", tx.id, tx.status, tx.amount, tx.asset_code);
        }
    }

    if let Some(cursor) = page.meta.next_cursor {
        if page.meta.has_more {
            let _next = client
                .transactions()
                .list(ListParams {
                    cursor: Some(cursor),
                    ..Default::default()
                })
                .await;
        }
    }
}
```

Full pagination example: [transactions_list.rs](../sdks/rust/examples/transactions_list.rs).

---

## Verify the walkthrough

### 1. Server health

```bash
curl -s "$SYNAPSE_API_URL/health" | jq .
```

### 2. CLI list (HTTP)

```bash
synapse-core tx list --limit 3 --format json | jq '.data | length'
```

Expect a non-negative integer (zero is valid on an empty database).

### 3. SDK list example

```bash
cd sdks/rust
cargo run --example transactions_list -- 5
```

### 4. SDK subscribe example

In one terminal:

```bash
cd sdks/rust
cargo run --example events_subscribe
```

In another, trigger a status broadcast (integration tests use `tx_broadcast.send(...)` after standing up the app). On a live server, process a deposit callback so the transaction transitions to `completed`.

### 5. Automated checks

```bash
# CLI parser smoke test (ignored unless --ignored)
cargo test --test cli_test test_cli_tx_list_help -- --ignored

# SDK HTTP integration tests (wiremock)
cd sdks/rust && cargo test --lib resources::transactions
```

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---------|--------------|-----|
| `connection refused` on CLI | Server not running | Start `synapse-core` per [setup.md](setup.md) |
| Empty `data` array | No transactions yet | Seed via callback or migrations/fixtures |
| WebSocket 401 | Missing/empty `token` | Use `?token=<non-empty>` |
| Cursor 400 | Stale cursor | Restart list from page 1 |
| SDK `InvalidCursor` | Same as above | Surface error; do not retry the same cursor |

---

## Related docs

- [CLI reference](CLI.md) — all `synapse-core tx` flags
- [API reference](api-reference.md) — `/transactions` and callback endpoints
- [Rust SDK README](../sdks/rust/README.md) — client construction and error types
- [WebSocket pagination](websocket-pagination.md) — resync and backpressure
