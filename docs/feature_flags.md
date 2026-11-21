# Feature Flags System

## Overview
The feature flags system allows toggling functionality at runtime without redeploying the application. This enables safe rollouts of new features and experimental functionality.

## Architecture

### Components

1. **Database Tables**
   - `feature_flags` — flag definitions (see [Storage Shape](#storage-shape))
   - `feature_flag_audit_logs` — append-only history of flag changes

2. **FeatureFlagService** (`src/services/feature_flags.rs`)
   - Holds a `sqlx::PgPool`; every check reads the database directly
   - No in-memory cache and no background refresh — each call is one query
   - Cheap enough for request paths, but not free: prefer checking a flag once
     per request and reusing the result

3. **Admin handlers** (`src/handlers/admin/mod.rs`)
   - `get_flags` — list flags (`GET /admin/flags`)
   - `update_flag` — toggle a flag (`PUT /admin/flags/:name`, body `{"enabled": bool}`)

## API

All functions are methods on `FeatureFlagService`, available as
`state.feature_flags` in handlers. Every one returns `Result<_, sqlx::Error>`.

| Function | Signature | Notes |
| --- | --- | --- |
| `new` | `fn new(pool: PgPool) -> Self` | Constructs the service |
| `is_enabled_ignoring_rollout` | `async fn is_enabled_ignoring_rollout(&self, flag_name: &str) -> Result<bool, sqlx::Error>` | Returns `enabled` only; **ignores `rollout_percentage`**. Unknown flag → `false` |
| `is_enabled_for_tenant` | `async fn is_enabled_for_tenant(&self, flag_name: &str, tenant_id: &str) -> Result<bool, sqlx::Error>` | Applies rollout using the tenant id as the scope key |
| `is_enabled_for_key` | `async fn is_enabled_for_key(&self, flag_name: &str, key: &str) -> Result<bool, sqlx::Error>` | Applies rollout using an arbitrary stable key (tenant id, stellar account, …) |
| `get_all_flags` | `async fn get_all_flags(&self) -> Result<HashMap<String, bool>, sqlx::Error>` | Name → enabled map |
| `get_all` | `async fn get_all(&self) -> Result<HashMap<String, bool>, sqlx::Error>` | Alias of `get_all_flags` |
| `update` | `async fn update(&self, name: &str, enabled: bool) -> Result<FeatureFlag, sqlx::Error>` | Sets `enabled`, writes an audit row, returns the updated flag |
| `update_rollout_percentage` | `async fn update_rollout_percentage(&self, name: &str, percentage: i32) -> Result<FeatureFlag, sqlx::Error>` | Sets `rollout_percentage` (0–100), writes an audit row |
| `get_audit_history` | `async fn get_audit_history(&self, flag_name: Option<&str>, limit: i64, offset: i64) -> Result<Vec<FlagAuditEntry>, sqlx::Error>` | Newest first; `None` returns all flags |
| `get_dependency_graph` | `async fn get_dependency_graph(&self) -> Result<HashMap<String, Vec<String>>, sqlx::Error>` | Flag name → `depends_on` list |

`hash_key_flag` is a private helper used by `is_enabled_for_key`; it is not part
of the public API.

### Rollout semantics

`is_enabled_for_key` (and therefore `is_enabled_for_tenant`) returns `false`
when the flag is missing or disabled. When the flag is enabled and
`rollout_percentage` is set below 100, the key is hashed together with the flag
name and included only if the hash falls inside the percentage — the same key
always gets the same answer for a given flag, so rollouts are stable across
calls. `is_enabled_ignoring_rollout` skips this entirely and should only be used
for process-wide toggles with no natural per-caller key.

## Usage

### Checking Feature Flags

```rust
// In your handler or service
if state
    .feature_flags
    .is_enabled_ignoring_rollout("experimental_processor")
    .await?
{
    // Use experimental logic
} else {
    // Use stable logic
}
```

For anything gating per-tenant or per-account behavior, use the scoped variant:

```rust
let enabled = state
    .feature_flags
    .is_enabled_for_tenant("new_asset_support", &tenant_id)
    .await?;
```

See `src/handlers/feature_flag_examples.rs` for complete handler examples
(`example_handler`, `new_asset_handler`, `advanced_handler`).

### Managing Flags via API

**List all flags:**
```bash
curl http://localhost:3000/admin/flags
```

**Enable a flag:**
```bash
curl -X PUT http://localhost:3000/admin/flags/experimental_processor \
  -H "Content-Type: application/json" \
  -d '{"enabled": true}'
```

**Disable a flag:**
```bash
curl -X PUT http://localhost:3000/admin/flags/new_asset_support \
  -H "Content-Type: application/json" \
  -d '{"enabled": false}'
```

### Adding New Feature Flags

Add new flags via SQL:

```sql
INSERT INTO feature_flags (name, enabled, description, rollout_percentage, depends_on)
VALUES ('my_new_feature', false, 'Description of the feature', 100, '{}');
```

Or via the database migration when deploying new features.

## Storage Shape

`feature_flags` (migrations `20250218000000_feature_flags`,
`20260426000001_feature_flag_rollout`, `20260428000002_feature_flag_dependencies`):

| Column | Type | Notes |
| --- | --- | --- |
| `name` | `VARCHAR(100)` | Primary key |
| `enabled` | `BOOLEAN NOT NULL DEFAULT false` | Indexed (`idx_feature_flags_enabled`) |
| `description` | `TEXT` | Nullable |
| `updated_at` | `TIMESTAMPTZ NOT NULL DEFAULT NOW()` | |
| `rollout_percentage` | `INT DEFAULT 100` | `CHECK (0..=100)`, indexed |
| `depends_on` | `TEXT[] DEFAULT '{}'` | GIN-indexed; read via `get_dependency_graph` |

`feature_flag_audit_logs` (migration `20260426000002_feature_flag_audit`):

| Column | Type | Notes |
| --- | --- | --- |
| `id` | `UUID PRIMARY KEY DEFAULT gen_random_uuid()` | |
| `flag_name` | `VARCHAR(100) NOT NULL` | Indexed |
| `old_value` | `JSONB` | e.g. `{"enabled": false}` |
| `new_value` | `JSONB` | e.g. `{"enabled": true}` |
| `actor` | `VARCHAR(255) NOT NULL` | Currently written as `"admin"` |
| `timestamp` | `TIMESTAMPTZ NOT NULL DEFAULT NOW()` | Indexed descending |
| `created_at` | `TIMESTAMPTZ NOT NULL DEFAULT NOW()` | |

Rust mirrors: `FeatureFlag { name, enabled, description, rollout_percentage,
depends_on }` and `FlagAuditEntry { id, flag_name, old_value, new_value, actor,
timestamp }`.

## Default Feature Flags

- `experimental_processor` - Enable experimental transaction processor logic
- `new_asset_support` - Enable support for new asset types

## Cache Behavior

- There is no cache: every check issues a `SELECT` against `feature_flags`
- `update` / `update_rollout_percentage` take effect immediately for all callers
- If the database is unavailable, checks return `Err(sqlx::Error)` — callers
  decide whether to fail open or closed

## Performance

- One indexed primary-key lookup per check
- No background refresh task and no in-memory state to keep consistent
- Batch repeated checks for the same flag within a request

## Security Considerations

- Admin endpoints should be protected with authentication (future enhancement)
- Consider rate limiting on admin endpoints
- Flag changes are recorded in `feature_flag_audit_logs` with actor `"admin"`

## Future Enhancements

- [ ] Authentication/authorization for admin endpoints
- [ ] Real actor identity in audit entries (currently hard-coded `"admin"`)
- [ ] Per-environment flag overrides
- [ ] Dependency-aware evaluation (`depends_on` is stored but not enforced)
- [ ] Flag expiration dates
- [ ] Metrics on flag usage
