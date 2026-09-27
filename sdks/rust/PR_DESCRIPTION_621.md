# PR: Document `transactions.search(filters)` with rustdoc and runnable example

## Summary

This PR adds comprehensive rustdoc comments and a new runnable example file for `transactions.search(filters)` in the Rust SDK (`sdks/rust/`). It also fixes underlying compilation issues in the SDK (missing error variants, client methods, module declarations) that were preventing examples and tests from building.

## Changes

### 1. Enhanced rustdoc for `Transactions::search()` (`sdks/rust/src/resources/transactions.rs`)

- Expanded the doc comment with a dedicated **Zero matches** section explaining that an empty result is returned as `Ok(TransactionSearch { total: 0, results: [], .. })`, not an error.
- Added a second `no_run` example specifically demonstrating the zero-matches case, showing callers they can inspect `page.total` and `page.results.is_empty()` without error matching.

### 2. New runnable example (`sdks/rust/examples/transactions_search.rs`)

- Demonstrates three key scenarios:
  1. **Filtering**: search `completed` USD transactions with `min_amount` filter
  2. **Pagination**: follow `next_cursor` through multi-page result sets
  3. **Zero matches**: search for a non-existent status and handle the successful empty response
- Follows existing example conventions (`SYNAPSE_API_URL`/`SYNAPSE_API_KEY` env vars, `#[tokio::main]`, descriptive doc-comment header).
- Compiles with `cargo build --example transactions_search`.

### 3. Fixed missing error variants (`sdks/rust/src/error.rs`)

Added the following `SynapseError` variants that the transactions resource code depends on:
- `Api { status: u16, message: String }` — raw API error returned by `SynapseClient::get()` / `get_query()`
- `NotFound(String)` — 404 response, matched in `Transactions::get()`
- `InvalidCursor(String)` — 400 with "cursor" in message, matched in `Transactions::list()` and `Transactions::search()`
- `Decode(String)` — JSON deserialization error
- Updated `is_transient()` to include `Api` (5xx statuses are retryable)

### 4. Added client methods (`sdks/rust/src/client.rs`)

- `SynapseClient::new(base_url, api_key)` — convenience constructor wrapping `builder().build()`
- `SynapseClient::transactions()` — accessor for the `Transactions` resource
- `SynapseClient::get_query(path, query)` — GET with query parameters (required by `search()` and `list()`)
- Updated `get()` to return `SynapseError::Api` instead of `SynapseError::Http` to match the error handling patterns in the transactions resource

### 5. Module declarations and re-exports (`sdks/rust/src/lib.rs`)

- Added `pub mod models;` and `pub mod resources;`
- Re-exported `SynapseClient`, `SynapseError`, `SearchParams`, `ListParams` at the crate root for ergonomic imports matching the existing examples' `use` statements

### 6. Dependencies (`sdks/rust/Cargo.toml`)

- Added `chrono = { version = "0.4", features = ["serde"] }` — required by `models.rs`
- Added `wiremock = "0.6"` under `[dev-dependencies]` — required by unit tests in `transactions.rs`

## Verification

All tests pass:

```
cargo test
> 11 passed; 0 failed
> 5 doc-tests passed; 0 failed

cargo build --example transactions_search
> Finished dev profile

cargo run --example transactions_search
> (connects to configured API or defaults — compiles successfully)
```

## Files touched (all under `sdks/rust/`)

| File | Change |
|------|--------|
| `Cargo.toml` | Added `chrono` and `wiremock` deps |
| `Cargo.lock` | Auto-updated |
| `src/client.rs` | Added `new()`, `transactions()`, `get_query()`, `get()` → `Api` |
| `src/error.rs` | Added `Api`, `NotFound`, `InvalidCursor`, `Decode` variants |
| `src/lib.rs` | Added `models`/`resources` modules, re-exports |
| `src/resources/transactions.rs` | Enhanced `search` rustdoc with zero-matches emphasis and second example |
| `examples/transactions_search.rs` | **New** — runnable example |

Closes #621
