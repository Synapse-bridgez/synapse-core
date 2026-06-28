# Database Input Validation

This document describes how input validation is applied to sqlx queries in
`src/db/`, the security guarantees each layer provides, and the known
limitations that callers must be aware of.

## Validation layers

User input passes through four layers before it reaches the database:

```
HTTP request
    │
    ▼
1. HTTP handler validation   (src/validation/mod.rs)
    │  field lengths, allow-lists, character sets, type parsing
    ▼
2. Type system               (Uuid, BigDecimal, DateTime<Utc>, TransactionStatus)
    │  invalid values rejected at parse time, not at query time
    ▼
3. sqlx bind parameters      (src/db/queries.rs)
    │  $1 / $2 placeholders — values sent over wire protocol, never interpolated
    ▼
4. Query timeouts             (src/db/queries.rs — with_timeout)
       per-tier deadlines prevent slow-plan DoS
```

## Layer 1 — HTTP handler validation (`src/validation/mod.rs`)

All user-supplied strings are validated before the database layer is reached.

| Field | Constraint | Constant |
|---|---|---|
| `stellar_account` | Exactly 56 characters | `STELLAR_ACCOUNT_LEN` |
| `asset_code` | Max 12 characters, allow-list (`USD`, …) | `ASSET_CODE_MAX_LEN`, `ALLOWED_ASSET_CODES` |
| `anchor_transaction_id` | Max 255 characters | `ANCHOR_TRANSACTION_ID_MAX_LEN` |
| `callback_type` | Max 20 characters | `CALLBACK_TYPE_MAX_LEN` |
| `callback_status` | Max 20 characters | `CALLBACK_STATUS_MAX_LEN` |
| `amount` | Max 64 characters, parsed to `BigDecimal` | `AMOUNT_INPUT_MAX_LEN` |

Control characters are stripped by `sanitize_string()` before any field is
stored.  This prevents log injection and display corruption.

## Layer 2 — Type system

Function signatures in `src/db/queries.rs` use strong types instead of raw
strings wherever possible:

```rust
// ✓ Correct — Uuid is parsed before reaching the query
pub async fn get_transaction(pool: &PgPool, id: Uuid) -> Result<Transaction>

// ✓ Correct — BigDecimal rejects non-numeric input at parse time
pub async fn insert_transaction(pool: &PgPool, tx: &Transaction) -> Result<Transaction>
```

`TransactionStatus` is an enum; only the four known variants (`Pending`,
`Processing`, `Completed`, `Failed`) can be passed to status-filter queries.
An invalid string is rejected by `FromStr` before it reaches the database.

## Layer 3 — sqlx bind parameters (`src/db/queries.rs`)

Every query in `queries.rs` uses positional `$N` placeholders:

```rust
// ✓ Safe — value is bound, never interpolated
sqlx::query("SELECT 1 FROM tenants WHERE api_key = $1 AND is_active = true LIMIT 1")
    .bind(api_key)
    .fetch_optional(pool)
    .await?;
```

The sqlx driver transmits the query text and parameter values as separate
messages over the PostgreSQL wire protocol.  The database server never
concatenates them, so SQL injection is impossible regardless of what `api_key`
contains.

### Compile-time query verification

sqlx's `query!` / `query_as!` macros verify queries against a live database at
compile time (via `DATABASE_URL`).  This catches type mismatches and schema
drift before the binary is built.  The CI `unit-tests` job runs migrations
before the build step so that this verification always reflects the current
schema.

## Layer 4 — Query timeouts (`with_timeout`)

```rust
pub async fn with_timeout<F, T>(tier: QueryTier, sql_label: &str, fut: F) -> Result<T>
```

Every query is wrapped with a per-tier deadline:

| Tier | Default | Env override |
|---|---|---|
| `Read` | 5 s | `DB_TIMEOUT_READ_SECS` |
| `Write` | 10 s | `DB_TIMEOUT_WRITE_SECS` |
| `Admin` | 60 s | `DB_TIMEOUT_ADMIN_SECS` |

On timeout the connection is dropped (not returned to the pool) so that a
hung query cannot block other requests.  The `DB_QUERY_TIMEOUT_TOTAL` counter
is incremented for alerting.

The `sql_label` passed to `with_timeout` is a static string describing the
query (e.g. `"list_transactions"`).  **Parameter values are never logged**,
preventing sensitive data from appearing in log streams.

## `query_builder.rs` — known limitation

`src/db/query_builder.rs` builds SQL by string interpolation:

```rust
// ⚠ String interpolation — caller must validate inputs
self.filters.push(format!("status = '{}'", status));
self.filters.push(format!("asset_code = '{}'", asset_code));
```

This is a deliberate trade-off for the admin/reporting use-case where the
number of active filters is not known at compile time and cannot be expressed
with a fixed `$N` parameter list.

### Safe usage contract

The builder is safe **only** when:

1. String values (`status`, `asset_code`, `stellar_account`) have been
   validated against an allow-list or regex before being passed to the builder.
2. Numeric/date values (`BigDecimal`, `DateTime<Utc>`) are formatted by their
   `Display` implementations, which produce safe output.
3. The builder is **not** used in user-facing request paths without prior
   validation.

### Planned migration

The builder should be migrated to sqlx's `QueryBuilder` API:

```rust
// Target — parameterised dynamic query (no interpolation)
let mut qb = sqlx::QueryBuilder::new("SELECT * FROM transactions WHERE ");
qb.push("status = ").push_bind(status);
let query = qb.build_query_as::<Transaction>();
```

Until that migration is complete, all callers must validate inputs at the
boundary.

## Tenant isolation

All user-facing queries include a `tenant_id` filter derived from the
authenticated session, not from user input:

```rust
// tenant_id comes from the verified JWT/API-key, not from the request body
sqlx::query_as::<_, Transaction>(
    "SELECT * FROM transactions WHERE tenant_id = $1 AND id = $2"
)
.bind(tenant_id)   // from auth context
.bind(tx_id)       // from path parameter, parsed as Uuid
```

PostgreSQL Row-Level Security (RLS) policies provide a second enforcement
layer: even if application code omits the `tenant_id` filter, the database
will only return rows belonging to the current tenant context set by
`set_tenant_context()`.

## Audit logging

Every write query (`INSERT`, `UPDATE`, `DELETE`) is wrapped in a transaction
that also inserts a row into `audit_logs`.  The audit record stores:

- `entity_type` and `entity_id` — what was changed
- `action` — the operation performed
- `changed_by` — the authenticated user/tenant
- `changes` — a JSON diff of before/after values

Sensitive fields (e.g. `webhook_secret`) are redacted before being written to
the audit log.

## Security checklist for new queries

When adding a new query to `src/db/queries.rs`:

- [ ] Use `$N` bind parameters for all user-supplied values.
- [ ] Use strong types (`Uuid`, `BigDecimal`, `DateTime<Utc>`) in the function
      signature rather than `&str` where possible.
- [ ] Wrap with `with_timeout(QueryTier::Read | Write | Admin, "label", ...)`.
- [ ] Include `tenant_id` in the WHERE clause for all tenant-scoped tables.
- [ ] Log the SQL label, not parameter values, in error/timeout messages.
- [ ] Add a corresponding test in `src/db/models.rs` or `tests/integration_test.rs`.
