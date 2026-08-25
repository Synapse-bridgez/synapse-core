# synapse-sdk

Rust client SDK for the [Synapse API](https://github.com/Synapse-bridgez/synapse-core).

## Installation

Add the crate to your `Cargo.toml`:

```toml
[dependencies]
synapse-sdk = { path = "../sdks/rust" }   # until published to crates.io
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
```

## Authentication

Synapse uses API-key authentication. Two key types are issued:

| Key type  | Header       | Use case                         |
|-----------|--------------|----------------------------------|
| Public    | `X-API-Key`  | Standard integrator requests     |
| Admin     | `X-API-Key`  | Privileged operations (higher-trust endpoints) |

Pass whichever key your endpoint requires to `SynapseClient::builder`:

```rust,no_run
use synapse_sdk::client::SynapseClient;

// Public API key
let client = SynapseClient::builder("https://api.example.com", "pk_live_...")
    .build();

// Admin API key — same builder, different key value
let admin_client = SynapseClient::builder("https://api.example.com", "sk_admin_...")
    .build();
```

## Quickstart

### Building a client

```rust,no_run
use synapse_sdk::client::SynapseClient;

#[tokio::main]
async fn main() {
    let client = SynapseClient::builder(
        "https://api.example.com",
        std::env::var("SYNAPSE_API_KEY").expect("SYNAPSE_API_KEY not set"),
    )
    .build();
}
```

### Retry configuration

All requests are retried automatically on transient failures (network errors and
5xx responses). 4xx responses are returned immediately without retrying.

```rust,no_run
use synapse_sdk::client::SynapseClient;

// Custom retry settings: up to 5 attempts, 100 ms base delay.
let client = SynapseClient::builder("https://api.example.com", "pk_live_...")
    .max_attempts(5)
    .base_delay_ms(100)
    .build();

// Disable retries entirely (useful when you manage your own retry loop).
let client = SynapseClient::builder("https://api.example.com", "pk_live_...")
    .disable_retries()
    .build();
```

Delays use decorrelated jitter — each retry draws a random value in
`[base_delay, prev_delay * 3]`, capped at 10 s — so concurrent callers spread
their retries instead of retrying in lockstep.

### Fetching a resource

Use `client.get::<T>(path)` for any endpoint that returns JSON:

```rust,no_run
use serde::Deserialize;
use synapse_sdk::client::SynapseClient;

#[derive(Deserialize)]
struct Transaction {
    id: String,
    amount: String,
    status: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = SynapseClient::builder("https://api.example.com", "pk_live_...")
        .build();

    let tx: Transaction = client.get("/v1/transactions/txn_abc123").await?;
    println!("Transaction {}: {} ({})", tx.id, tx.amount, tx.status);
    Ok(())
}
```

### Cursor pagination

List endpoints return a page of items plus an optional `next_cursor`. Use
[`PageIter`](src/pagination.rs) to iterate lazily — each call to `next_page`
issues exactly one request:

```rust,no_run
use serde::Deserialize;
use synapse_sdk::client::SynapseClient;
use synapse_sdk::pagination::PageIter;

#[derive(Deserialize)]
struct Transaction {
    id: String,
    amount: String,
}

#[derive(Deserialize)]
struct TransactionPage {
    transactions: Vec<Transaction>,
    next_cursor: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = SynapseClient::builder("https://api.example.com", "pk_live_...")
        .build();

    let mut iter = PageIter::new(|cursor| {
        let client = client.clone();
        async move {
            let path = match cursor {
                Some(c) => format!("/v1/transactions?cursor={c}&limit=50"),
                None    => "/v1/transactions?limit=50".to_string(),
            };
            let page: TransactionPage = client.get(&path).await?;
            Ok((page.transactions, page.next_cursor))
        }
    });

    while let Some(page) = iter.next_page().await {
        for tx in page? {
            println!("{}: {}", tx.id, tx.amount);
        }
    }
    Ok(())
}
```

### Error handling

```rust,no_run
use synapse_sdk::client::SynapseClient;
use synapse_sdk::error::SynapseError;

#[tokio::main]
async fn main() {
    let client = SynapseClient::builder("https://api.example.com", "pk_live_...")
        .build();

    match client.get::<serde_json::Value>("/v1/transactions/missing").await {
        Ok(body)  => println!("got: {body}"),
        Err(SynapseError::Http { status: 404, .. }) => println!("not found"),
        Err(SynapseError::Http { status, body })    => println!("HTTP {status}: {body}"),
        Err(SynapseError::Network(e))               => println!("network: {e}"),
    }
}
```

## Module overview

| Module                      | Contents                                      |
|-----------------------------|-----------------------------------------------|
| `synapse_sdk::client`       | `SynapseClient` and `SynapseClientBuilder`    |
| `synapse_sdk::error`        | `SynapseError` enum                           |
| `synapse_sdk::retry`        | `retry_with_backoff` (used internally)        |
| `synapse_sdk::pagination`   | `PageIter` lazy cursor-pagination iterator    |

## License

MIT
