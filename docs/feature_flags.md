# Feature Flags System

## Overview
The feature flags system allows toggling functionality at runtime without redeploying the application. This enables safe rollouts of new features and experimental functionality.

## Architecture

### Components

1. **Database Table** (`feature_flags`)
   - Stores flag definitions with name, enabled status, description, and timestamp
   - Indexed for fast lookups

2. **FeatureFlagService** (`src/services/feature_flags.rs`)
   - In-memory cache for fast flag checks
   - Periodic refresh from database (default: 1 hour)
   - Thread-safe using RwLock

3. **Admin API** (`src/handlers/admin.rs`)
   - GET `/admin/flags` - List all feature flags
   - PUT `/admin/flags/:name` - Update flag status

## Usage

### Checking Feature Flags

```rust
// In your handler or service
if state.feature_flags.is_enabled("experimental_processor").await {
    // Use experimental logic
} else {
    // Use stable logic
}
```

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
INSERT INTO feature_flags (name, enabled, description)
VALUES ('my_new_feature', false, 'Description of the feature');
```

Or via the database migration when deploying new features.

## Default Feature Flags

- `experimental_processor` - Enable experimental transaction processor logic
- `new_asset_support` - Enable support for new asset types

## Cache Behavior

- Cache refreshes every 1 hour by default
- Updates via API immediately update the cache
- If database is unavailable, cache continues serving last known values
- On startup, cache is populated before accepting requests

## Performance

- Flag checks are O(1) in-memory lookups
- No database queries during normal operation
- Minimal overhead (~nanoseconds per check)

## Security Considerations

- Admin endpoints should be protected with authentication (future enhancement)
- Consider rate limiting on admin endpoints
- Audit log for flag changes (future enhancement)

## Future Enhancements

- [ ] Authentication/authorization for admin endpoints
- [ ] Audit logging for flag changes
- [ ] Per-environment flag overrides
- [ ] Gradual rollout (percentage-based flags)
- [ ] Flag expiration dates
- [ ] Metrics on flag usage
