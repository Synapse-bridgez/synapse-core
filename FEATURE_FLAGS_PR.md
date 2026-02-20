# Pull Request: Feature Flag System (Infrastructure)

## Issue
Resolves #28 - Feature Flag System (Infrastructure)

## Summary
Implemented a database-backed feature flag system with in-memory caching for runtime feature toggling without redeployment. The system enables safe rollouts of new features and experimental functionality per-environment.

## Changes Made

### 1. Database Migration (`migrations/20250218000000_feature_flags.sql`)
- Created `feature_flags` table with columns:
  - `name` (VARCHAR PRIMARY KEY) - Unique flag identifier
  - `enabled` (BOOLEAN) - Flag status
  - `description` (TEXT) - Human-readable description
  - `updated_at` (TIMESTAMPTZ) - Last modification timestamp
- Added index on `enabled` column for fast queries
- Seeded default flags:
  - `experimental_processor` - For experimental transaction logic
  - `new_asset_support` - For new asset type support

### 2. Feature Flag Service (`src/services/feature_flags.rs`)
- **FeatureFlagService** with in-memory cache using `Arc<RwLock<HashMap>>`
- **Key Methods:**
  - `new(pool)` - Initialize service with database pool
  - `start(refresh_interval_hours)` - Start background refresh task
  - `refresh_cache()` - Load all flags from database into cache
  - `is_enabled(name)` - Fast O(1) flag check from cache
  - `get_all()` - Retrieve all flags with metadata
  - `update(name, enabled)` - Update flag and refresh cache immediately
- Background task refreshes cache every 1 hour (configurable)
- Thread-safe concurrent access via RwLock
- Includes unit tests for flag creation

### 3. Admin API Handlers (`src/handlers/admin.rs`)
- **GET `/admin/flags`** - List all feature flags with metadata
  - Returns JSON array of flags with name, enabled, description, updated_at
- **PUT `/admin/flags/:name`** - Update specific flag
  - Request body: `{"enabled": true|false}`
  - Returns updated flag on success
  - Returns 404 if flag doesn't exist
- Proper error handling with appropriate HTTP status codes

### 4. Integration (`src/main.rs`)
- Added `services` module to project structure
- Integrated `FeatureFlagService` into `AppState`
- Initialize service on startup:
  - Load initial cache before accepting requests
  - Start background refresh task (1-hour interval)
- Registered admin routes in router
- Added logging for service initialization

### 5. Documentation (`docs/feature_flags.md`)
- Comprehensive guide covering:
  - Architecture and components
  - Usage examples in code
  - API endpoint documentation with curl examples
  - Adding new feature flags
  - Cache behavior and performance characteristics
  - Security considerations
  - Future enhancement roadmap

## Technical Details

### Architecture Pattern
- **Cache-Aside Pattern**: In-memory cache with periodic refresh
- **Thread-Safe**: RwLock allows multiple concurrent readers
- **Fail-Safe**: Cache continues serving if database unavailable
- **Immediate Updates**: API updates refresh cache instantly

### Performance
- Flag checks: O(1) in-memory HashMap lookup (~nanoseconds)
- No database queries during normal operation
- Minimal memory footprint (flags stored as HashMap<String, bool>)
- Background refresh runs asynchronously without blocking

### Cache Strategy
- **Initial Load**: Populated on startup before accepting requests
- **Periodic Refresh**: Every 1 hour (configurable)
- **Immediate Update**: API changes update cache synchronously
- **Consistency**: Eventually consistent (1-hour max staleness)

## API Examples

### List All Flags
```bash
curl http://localhost:3000/admin/flags
```

Response:
```json
[
  {
    "name": "experimental_processor",
    "enabled": false,
    "description": "Enable experimental transaction processor logic",
    "updated_at": "2025-02-18T10:00:00Z"
  },
  {
    "name": "new_asset_support",
    "enabled": false,
    "description": "Enable support for new asset types",
    "updated_at": "2025-02-18T10:00:00Z"
  }
]
```

### Enable a Feature
```bash
curl -X PUT http://localhost:3000/admin/flags/experimental_processor \
  -H "Content-Type: application/json" \
  -d '{"enabled": true}'
```

Response:
```json
{
  "name": "experimental_processor",
  "enabled": true,
  "description": "Enable experimental transaction processor logic",
  "updated_at": "2025-02-18T12:30:00Z"
}
```

## Usage in Code

```rust
// In any handler or service with access to AppState
pub async fn process_transaction(State(state): State<AppState>) -> impl IntoResponse {
    if state.feature_flags.is_enabled("experimental_processor").await {
        // Use new experimental logic
        experimental_process(&state).await
    } else {
        // Use stable production logic
        stable_process(&state).await
    }
}
```

## Testing

### Manual Testing

1. **Start PostgreSQL**:
```bash
docker run --name synapse-postgres \
  -e POSTGRES_USER=synapse \
  -e POSTGRES_PASSWORD=synapse \
  -e POSTGRES_DB=synapse \
  -p 5432:5432 -d postgres:14-alpine
```

2. **Run application**:
```bash
DATABASE_URL=postgres://synapse:synapse@localhost:5432/synapse cargo run
```

3. **Verify initialization**:
Check logs for:
- "Database migrations completed"
- "Feature flags service initialized"

4. **Test API endpoints**:
```bash
# List flags
curl http://localhost:3000/admin/flags

# Enable a flag
curl -X PUT http://localhost:3000/admin/flags/experimental_processor \
  -H "Content-Type: application/json" \
  -d '{"enabled": true}'

# Verify update
curl http://localhost:3000/admin/flags
```

5. **Test cache refresh**:
```sql
-- Update flag directly in database
UPDATE feature_flags SET enabled = false WHERE name = 'experimental_processor';

-- Wait 1 hour or restart service to see cache refresh
```

### Automated Tests
```bash
cargo test
```

Unit tests included for:
- FeatureFlag struct creation
- Service initialization

## Breaking Changes
None. This is a new feature with no impact on existing functionality.

## Security Notes
- Admin endpoints currently have no authentication (intentional for Phase 1)
- Should add authentication middleware before production deployment
- Consider rate limiting on admin endpoints
- Audit logging recommended for flag changes

## Future Enhancements
- [ ] Authentication/authorization for admin endpoints
- [ ] Audit trail for flag changes
- [ ] Per-environment flag overrides
- [ ] Percentage-based gradual rollouts
- [ ] Flag expiration dates
- [ ] Prometheus metrics for flag usage
- [ ] WebSocket notifications for real-time flag updates

## Checklist
- [x] Code follows project style guidelines
- [x] Migration tested locally
- [x] Documentation created (docs/feature_flags.md)
- [x] Admin API endpoints implemented
- [x] In-memory cache with periodic refresh
- [x] Helper function `is_enabled()` available
- [x] Unit tests included
- [x] No breaking changes
- [x] Logging added for key operations

## Dependencies
No new external dependencies required. Uses existing:
- `sqlx` for database operations
- `tokio` for async runtime and background tasks
- `axum` for HTTP endpoints

## Notes
- Cache refresh interval set to 1 hour (configurable in main.rs)
- Default flags seeded in migration for immediate use
- Service initializes cache before accepting HTTP requests
- Flag updates via API immediately update cache (no wait for refresh)

## References
- Issue #28: Feature Flag System (Infrastructure)
- Similar pattern used in Issue #21 (referenced in requirements)
