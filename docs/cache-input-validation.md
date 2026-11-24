# Cache Input Validation (Redis)

## Overview

The caching layer validates keys, values, TTLs, and invalidation patterns **before** issuing Redis commands. Validation lives in `src/cache/validation.rs` and is applied at the boundary of [`QueryCache`](../src/services/query_cache.rs) (`get`, `set`, `invalidate`, `invalidate_exact`).

This keeps malformed or hostile inputs from reaching Redis, bounds memory use per entry, and aligns with validation patterns used elsewhere in the codebase (for example `src/auth/input_validation.rs` and `src/telemetry/input_validation.rs`).

## Components

| Component | Location | Role |
|-----------|----------|------|
| `CacheValidator` | `src/cache/validation.rs` | Static validation API |
| `ValidationError` | `src/cache/validation.rs` | Typed errors via `thiserror` |
| `QueryCache` integration | `src/services/query_cache.rs` | Validates on every public Redis operation |
| Module exports | `src/cache/mod.rs` | Re-exports validator and limits |
| `CacheConfig` | `src/services/query_cache.rs` | Cache TTL and sizing settings; also exported as `synapse_core::CacheConfig` |

## Validation Rules

### Cache keys (`validate_key`)

Used for `GET`, `SET`, and exact `DEL` operations.

| Rule | Value |
|------|-------|
| Minimum length | 1 (non-empty) |
| Maximum length | 512 bytes (UTF-8) |
| Allowed characters | `A–Z`, `a–z`, `0–9`, `_`, `:`, `-` |

**Examples**

| Key | Result |
|-----|--------|
| `query:status_counts` | Valid |
| `query:daily_totals:7` | Valid |
| `query:asset_total:USD` | Valid |
| `` (empty) | Rejected |
| `key with spaces` | Rejected |
| `key@invalid` | Rejected |
| 513-byte key | Rejected (`KeyTooLong`) |

Internal helpers such as `cache_key_status_counts()` in `query_cache.rs` already produce keys that satisfy these rules.

### Invalidation patterns (`validate_pattern`)

Used for `invalidate` (Redis `KEYS` + `DEL`).

| Rule | Value |
|------|-------|
| Same length and charset as keys | Yes |
| Wildcard | Optional single trailing `*` only |

**Examples**

| Pattern | Result |
|---------|--------|
| `query:status_counts` | Valid (exact match) |
| `query:daily_totals:*` | Valid |
| `query:*:totals` | Rejected |
| `*` | Rejected |

### Value size (`validate_value_size`)

Applied to the **serialized** JSON payload in `QueryCache::set` after `serde_json::to_string`.

| Rule | Value |
|------|-------|
| Maximum size | 512 MiB |

### TTL (`validate_ttl`)

Applied in `QueryCache::set` from `Duration::as_secs()`.

| Rule | Value |
|------|-------|
| Minimum | 1 second |
| Maximum | `i64::MAX` seconds (sanity cap on duration conversion) |

`None` TTL is not used by `QueryCache::set`; `validate_entry` accepts `None` for callers that validate key/value only.

## Integration Flow

```
Request (key / pattern / value / ttl)
        │
        ▼
┌───────────────────┐
│  CacheValidator   │  ← fail fast, no Redis I/O
└─────────┬─────────┘
          │ Ok
          ▼
┌───────────────────┐
│  QueryCache       │  memory LRU → Redis (circuit breaker)
└───────────────────┘
```

On failure, `QueryCache` returns `redis::RedisError` with kind `TypeError` and message prefix `cache validation failed`.

## Security

1. **Key namespace hygiene** — Restricted character set reduces risk of delimiter injection or unexpected key shapes in shared Redis instances.
2. **Size limits** — Key and value caps mitigate DoS via huge keys or payloads.
3. **TTL sanity** — Non-positive TTL is rejected so expiry behavior stays predictable.
4. **Pattern safety** — Only a trailing `*` is allowed, limiting glob-style invalidation to prefix namespaces (e.g. `query:daily_totals:*`).

Validation does **not** replace tenant isolation or auth; idempotency and other Redis users may use separate key layouts. New cache features should use the `query:` prefix and `CacheValidator` at boundaries.

## Performance

| Check | Cost |
|-------|------|
| `validate_key` | O(n) over key bytes, no heap allocation on success |
| `validate_pattern` | O(n), same as key |
| `validate_value_size` | O(1) (`len()` only) |
| `validate_ttl` | O(1) |

Validation runs on every `get`/`set`/`invalidate` call. For typical keys (&lt; 64 bytes) overhead is negligible compared to Redis RTT.

## Usage

### Direct API

```rust
use synapse_core::cache::{CacheValidator, ValidationError};

// Before custom Redis operations
CacheValidator::validate_key("query:custom:metric")?;

let payload = serde_json::to_vec(&value)?;
CacheValidator::validate_entry("query:custom:metric", &payload, Some(300))?;
```

### Via QueryCache

```rust
// Invalid key — returns Err before Redis
cache.get::<MyType>("bad key").await?;

// Invalid pattern — returns Err before KEYS
cache.invalidate("bad@pattern").await?;
```

Built-in keys from `cache_key_*()` helpers always pass validation.

## Adding New Cache Operations

When adding a Redis-backed cache path, keep validation at the public boundary so invalid input fails before memory-cache mutation, circuit-breaker execution, or Redis I/O:

1. Validate exact keys with `CacheValidator::validate_key`.
2. Validate prefix invalidation inputs with `CacheValidator::validate_pattern`.
3. Validate the serialized payload bytes with `CacheValidator::validate_value_size`.
4. Validate caller-supplied TTLs with `CacheValidator::validate_ttl`, or use `validate_entry` when key, value, and optional TTL are available together.
5. Convert `ValidationError` into the cache layer's external error type without leaking raw payload contents.

Prefer helper functions for generated keys (for example `cache_key_status_counts`) so callers do not need to assemble Redis key strings by hand.

## Error Types

```rust
pub enum ValidationError {
    InvalidKey(String),
    InvalidValue(String),
    InvalidPattern(String),
    KeyTooLong,
    ValueTooLarge,
    InvalidTTL,
}
```

Constants: `MAX_KEY_LENGTH` (512), `MAX_VALUE_SIZE` (512 MiB).

## Testing

```bash
# Unit tests for validator
cargo test cache::validation

# Query cache integration (includes invalid key/pattern rejection)
cargo test query_cache
```

Coverage includes:

- Valid and invalid keys (empty, charset, length boundaries)
- Pattern wildcards (trailing `*` only)
- Value size boundary at 512 MiB
- TTL edge cases (0, negative, positive)
- Combined `validate_entry`
- `QueryCache::get` / `invalidate` rejection without Redis

## Troubleshooting

| Symptom | Likely cause | Fix |
|---------|----------------|-----|
| `cache validation failed: Invalid key` | Spaces or special chars in key | Use `cache_key_*()` helpers or allowlisted charset |
| `Key too long` | Key &gt; 512 bytes | Shorten key or shard namespace |
| `Value too large` | Serialized JSON &gt; 512 MiB | Paginate or compress results before caching |
| `Invalid pattern` | `*` not at end or multiple `*` | Use `prefix:*` or exact key |
| `Invalid TTL` | Zero/sub-second `Duration` | Use `Duration::from_secs(1)` minimum |

## Related documentation

- [Query Result Caching](./query-cache.md) — TTLs, invalidation, metrics
- [GraphQL Idempotency](./graphql-idempotency.md) — separate Redis key validation for idempotency keys

## References

- Issue: #462 Document Input Validation in Caching
- Implementation: `src/cache/validation.rs`, `src/cache/mod.rs`, `src/services/query_cache.rs`
