# PR: Rust SDK test coverage for settlements.list() / settlements.get(id)

**Branch**: `feature/rust-sdk-test-coverage-for-settlementslist-settlementsgetid`

Closes #19

## Summary

Adds focused unit tests for `settlements.list()` and `settlements.get(id)` using a mocked HTTP transport (`wiremock`). No live server or database is required. Also wires the `Settlements` resource into the SDK client and fixes existing SDK compilation gaps.

## Changes

### Created Files
- `sdks/rust/src/resources/settlements.rs` — `Settlements` resource with `list()` and `get()` methods plus 4 tests

### Modified Files
| File | Changes |
|------|---------|
| `sdks/rust/Cargo.toml` | Added `wiremock` and `chrono` dependencies |
| `sdks/rust/src/client.rs` | Added `get_query()`, `settlements()`, `transactions()`, and `new()` constructor |
| `sdks/rust/src/error.rs` | Added `NotFound`, `InvalidCursor`, `Decode` variants |
| `sdks/rust/src/lib.rs` | Exported resources module and public types |
| `sdks/rust/src/models.rs` | Added `Settlement` and `SettlementListResponse` |
| `sdks/rust/src/resources/mod.rs` | Exported `settlements` module |
| `sdks/rust/src/resources/transactions.rs` | Fixed references to match current error variants |

## Tests

```bash
cargo test -p synapse-sdk --lib
```

All 15 tests pass (4 new + 11 existing):
- `get_returns_settlement_on_200` — happy path for single record fetch
- `get_returns_not_found_on_404` — maps HTTP 404 to `SynapseError::NotFound`
- `list_returns_page_on_200` — happy path for paginated list
- `list_returns_empty_page_on_zero_matches` — empty result set is not an error

## Scope Guard

Only files under `sdks/rust/` were modified. No changes were made to `src/`.
