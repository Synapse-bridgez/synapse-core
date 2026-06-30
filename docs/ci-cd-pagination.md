# Pagination in CI/CD

This document explains how pagination logic is tested and measured within the
GitHub Actions workflow (`.github/workflows/rust.yml`).

## Workflow overview

```
push / pull_request
        │
        ├── unit-tests          (Postgres only, no Redis)
        │       └── cargo test --lib --bins
        │
        ├── integration-tests   (Postgres + Redis)
        │       └── cargo test -- --ignored
        │
        └── coverage            (runs after both jobs pass)
                └── cargo llvm-cov  →  Codecov + threshold check
```

## Where pagination tests live

| Layer | Source file | Test location | Job |
|---|---|---|---|
| Cursor encode/decode | `src/utils/cursor.rs` | inline `#[cfg(test)]` | unit-tests |
| Query builder (WHERE clauses) | `src/db/query_builder.rs` | inline `#[cfg(test)]` | unit-tests |
| GraphQL cursor pagination | `src/graphql/pagination/cursor.rs` | inline `#[cfg(test)]` | unit-tests |
| GraphQL offset pagination | `src/graphql/pagination/offset.rs` | inline `#[cfg(test)]` | unit-tests |
| HTTP list endpoint | `src/handlers/webhook.rs` | `tests/integration_test.rs` | integration-tests |
| Full-text search pagination | `src/handlers/search.rs` | `tests/search_test.rs` | integration-tests |
| WebSocket history pagination | `src/handlers/ws.rs` | `tests/websocket_test.rs` | integration-tests |
| GraphQL resolver pagination | `src/graphql/resolvers/` | `tests/graphql_test.rs` | integration-tests |
| Streaming export (large pages) | `src/handlers/export.rs` | `tests/export_test.rs` | integration-tests |

## Job 1 — unit-tests

Runs `cargo test --lib --bins`. No Redis is needed.

Pagination logic covered:

- **Cursor encoding** — `(created_at, id)` tuples are base64-encoded for
  opaque client tokens. Tests verify round-trip correctness and rejection of
  malformed input.
- **Limit clamping** — limits are clamped to `[1, MAX_LIMIT]` before reaching
  the database. Tests verify boundary values (0 → 1, MAX+1 → MAX).
- **Direction flag** — `backward: bool` reverses the ORDER BY and comparison
  operator. Tests verify that forward and backward queries produce complementary
  result sets.
- **Filter composition** — `TransactionFilters` fields are combined with AND.
  Tests verify that each filter is applied independently and in combination.

## Job 2 — integration-tests

Runs `cargo test -- --ignored`. Requires live Postgres and Redis.

Pagination integration tests are gated with `#[ignore]` because they need a
real database with partitioned tables and seeded data.

### Skipped tests

```
--skip test_api_versioning_headers
--skip test_invalid_signature_flow
```

These tests are unrelated to pagination and are excluded to prevent flaky
failures from masking pagination regressions.

### What is tested end-to-end

**Cursor pagination** (`GET /transactions?limit=N&cursor=...`):
- First page (no cursor) returns the N most recent transactions.
- Subsequent pages use the cursor from the previous response.
- Requesting past the last page returns an empty array.
- Cursors from deleted records do not cause errors (graceful skip).

**Offset pagination** (`GET /transactions?page=N&page_size=M`, v2 API):
- Page 1 matches the first M records ordered by `created_at DESC`.
- Requesting a page beyond the last returns an empty array.
- `page_size` is clamped server-side; oversized values do not bypass limits.

**Search pagination** (`GET /transactions/search?q=...&cursor=...`):
- Full-text search results are paginated with the same cursor mechanism.
- Filters (status, asset, date range) are applied before pagination.

**WebSocket history** (`GET /ws`):
- On connect, the server sends a paginated history of recent transactions.
- The client can request additional pages via a `{"type":"paginate"}` message.

**GraphQL pagination**:
- `transactions(first: N, after: cursor)` — cursor-based forward pagination.
- `transactions(offset: N, limit: M)` — offset-based pagination.
- Both return `pageInfo.hasNextPage` and `pageInfo.endCursor` correctly.

**Streaming export** (`GET /export`):
- Large result sets are streamed in chunks; each chunk is a page of records.
- The export completes without OOM even for datasets larger than available memory.

## Job 3 — coverage

Runs after both previous jobs pass. Uses `cargo-llvm-cov` for LLVM-based
instrumentation.

### Two-pass collection

```bash
# Pass 1: unit tests
cargo llvm-cov --workspace --lib --bins --no-report

# Pass 2: integration tests
cargo llvm-cov --workspace --no-report -- --ignored \
  --skip test_api_versioning_headers \
  --skip test_invalid_signature_flow
```

Both passes write to the same coverage data directory so that lines exercised
only by integration tests (e.g. the actual SQL query execution paths) are
counted in the final report.

### Exclusion regex

```
(^|.*/)(tests?/.*|generated/.*|\.sqlx/.*)$
```

Test helper files (`tests/`) and generated code (`.sqlx/`) are excluded so
that only production pagination code counts toward the threshold.

### Thresholds

| Level | Threshold | Action |
|---|---|---|
| Error | < 40 % line coverage | CI fails |
| Warning | < 60 % line coverage | Warning annotation |

Adding new pagination code without corresponding tests will cause coverage to
drop. If the drop crosses 40 %, CI fails and the PR cannot be merged.

### Coverage artifacts

The coverage job uploads three artifacts:

| Artifact | Format | Use |
|---|---|---|
| `target/llvm-cov/html/` | HTML | Human-readable report |
| `target/llvm-cov/lcov.info` | LCOV | Uploaded to Codecov |
| `target/llvm-cov/summary.json` | JSON | Threshold enforcement |

The Codecov badge in `README.md` reflects the LCOV upload from this job.

## Adding new pagination tests

1. **Pure logic** (no I/O) → add an inline `#[cfg(test)]` module to the
   relevant `src/` file. It will run in `unit-tests` without any services.

2. **Database queries** → add a test to the appropriate file in `tests/` and
   annotate it with `#[ignore = "Requires Postgres"]`. It will run in
   `integration-tests`.

3. **Full HTTP flow** → add a test using `TestApp` from `tests/common/mod.rs`.
   `TestApp` spins up a real Postgres container via testcontainers, so no
   manual setup is needed.

4. **Verify locally**:
   ```bash
   # Unit tests only
   cargo test --lib --bins

   # Integration tests (requires Docker)
   cargo test -- --ignored
   ```

## Security considerations

- **Limit validation** is enforced in the handler before the query is built.
  CI tests verify that oversized limits are clamped, preventing DoS via
  unbounded queries.
- **Cursor validation** rejects malformed base64 or invalid UUID/timestamp
  pairs with a 400 response. CI tests cover both valid and invalid cursor
  inputs.
- **Tenant isolation** — all paginated queries include a `tenant_id` filter
  derived from the authenticated request. Integration tests verify that one
  tenant cannot page through another tenant's records.
