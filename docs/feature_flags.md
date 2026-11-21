# Feature Flags System

## Overview
The feature flags system allows toggling functionality at runtime without redeploying the application. This enables safe rollouts of new features and experimental functionality.

## Architecture

### Components

1. **Database Table** (`feature_flags`)
   - Stores flag definitions with name, enabled status, description, and timestamp
   - Indexed for fast lookups

2. **FeatureFlagService** (`src/services/feature_flags.rs`)
   - In-memory snapshot of the full flag set for fast flag checks
   - Periodic refresh from database (default: 60 seconds)
   - Manual refresh trigger for operators to force a reload
   - Thread-safe using RwLock

3. **Admin API** (`src/handlers/admin.rs`)
   - GET `/admin/flags` - List all feature flags
   - PUT `/admin/flags/:name` - Update flag status
   - POST `/admin/flags/refresh` - Force an immediate snapshot reload

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

**Force a snapshot reload:**
```bash
curl -X POST http://localhost:3000/admin/flags/refresh
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

- The flag set is loaded once into an in-memory snapshot and served from memory on every check
- The snapshot refreshes automatically every 60 seconds by default
- Updates via API immediately update the snapshot
- Operators can force an immediate reload via `POST /admin/flags/refresh`
- If database is unavailable, the snapshot continues serving last known values
- On startup, the snapshot is populated before accepting requests

### Stale Window

Because checks are served from the snapshot, a change made directly in the database
(e.g. via SQL, bypassing the admin API) is not visible until the next refresh. The
maximum staleness is the refresh interval (60 seconds by default). Changes made through
the admin API take effect immediately. If a change must be visible right away, call the
manual refresh endpoint after updating the database.

## Performance

- Flag checks are O(1) in-memory lookups
- No database queries during normal operation (one query per refresh interval)
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
