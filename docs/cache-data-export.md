# Cache Data Export

This document describes Redis-backed cache export considerations for the Caching module.

## Overview

The caching layer in `src/cache` is responsible for validating Redis keys, values, TTLs, and invalidation patterns before storage or retrieval. When data export involves cached entries, the same validation guarantees apply to ensure exported cache metadata is safe, consistent, and auditable.

## Redis Storage and Export Boundaries

Redis is used as a fast in-memory store for query results and webhook-related nonces. Exported cache metadata should never include raw request contents or unvalidated tokens.

Cache export workflows should behave as follows:

- Validate cache keys using [`CacheValidator::validate_key`].
- Validate cache entry values using [`CacheValidator::validate_value_size`].
- Validate TTLs using [`CacheValidator::validate_ttl`] when cache entries are set with expiration.
- Validate invalidation patterns using [`CacheValidator::validate_pattern`] before using them in Redis scan or delete operations.

## Security Considerations

- Do not export raw Redis values as part of security logs unless the values are sanitized and do not contain credentials or PII.
- Use validated cache keys to avoid Redis key injection attacks.
- Avoid exporting Redis command parameters directly in logs. Use structured metadata such as `cache_key`, `ttl_secs`, and `export_action` instead.
- Cache export should preserve the invariant that Redis keys contain only allowed characters (`[A-Za-z0-9_:-]`) and are limited to 512 bytes.

## Data Export Best Practices

- Export only provenance metadata, not full cached payloads, unless the cached payload is intentionally safe to serialize.
- Use `CacheValidator` before any cache operation to reject malformed values before they reach Redis.
- Ensure export code does not bypass the cache validation layer.

## Example

```rust
use synapse_core::cache::validation::CacheValidator;

let key = "query:status_counts";
let value = serde_json::to_vec(&payload).expect("serialize payload");

CacheValidator::validate_entry(key, &value, Some(3600))?;

// safe to export metadata about this entry now
let export_metadata = CacheExportMetadata {
    cache_key: key.to_string(),
    ttl_secs: Some(3600),
    size_bytes: value.len() as u64,
};
```

## Related Documentation

- [Cache input validation](./cache-input-validation.md)
