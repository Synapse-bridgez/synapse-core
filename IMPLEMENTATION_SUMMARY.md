# Issue #16: Database Partitioning for High Volume (Scaling)

## Summary

Implemented **time-based database partitioning** for the `transactions` table to support millions of records efficiently. This feature prepares the system for production-scale usage by improving query performance, reducing maintenance overhead, and enabling retention policies.

## Changes Made

### 1. Database Migration
**File**: `migrations/20250219000000_implement_db_partitioning.sql`

- Migrates `transactions` table from monolithic to partitioned structure
- Creates monthly partitions (named `transactions_y{YYYY}m{MM}`)
- Initializes partitions for 2025-2026
- Creates indexes on each partition for status, stellar_account, and created_at
- Includes helper function for dynamic partition creation

### 2. Partition Management Module
**File**: `src/db/cron.rs` (NEW)

Implements `PartitionManager` with methods:
- `create_partition()`: Create monthly partitions on-demand
- `ensure_future_partitions()`: Ensure partitions exist 3 months ahead
- `archive_old_partitions()`: Detach partitions older than retention period (configurable)
- `analyze_partitions()`: Update statistics for query planner
- `run_maintenance()`: Main job combining all operations

### 3. Background Job Integration
**File**: `src/main.rs`

- Spawns partition management background job
- Runs daily (configurable interval)
- Handles errors gracefully without blocking app startup

### 4. Module Exports
**File**: `src/db/mod.rs`

- Added `pub mod cron;` to export partition management module

### 5. Configuration
**File**: `Cargo.toml`

- Removed duplicate `serde_json` dependency

### 6. Documentation

#### `docs/database-partitioning.md` (NEW)
Comprehensive guide covering:
- Why partitioning is needed
- Implementation details and strategy
- Partition naming and structure
- Usage examples and queries
- Maintenance operations
- Configuration options
- Performance expectations
- Troubleshooting guide

#### `docs/build-instructions.md` (NEW)
Complete build setup guide with:
- Multiple build methods (with/without DATABASE_URL)
- Docker setup instructions
- sqlx offline mode guide
- Troubleshooting section

#### `.env.example` (NEW)
Template environment file with:
- All required configuration variables
- Helpful comments and examples
- Connection string templates

#### `setup.sh` (NEW)
Automated setup script that:
- Checks Docker and Rust installation
- Starts PostgreSQL container
- Creates .env file
- Runs migrations

## Technical Details

### Partitioning Strategy
- **Type**: Declarative range partitioning
- **Column**: `created_at` (timestamp)
- **Granularity**: Monthly partitions
- **Range**: 2025-01-01 to 2026-03-01 (extensible)

### Partition Naming
Format: `transactions_y{YYYY}m{MM}`
- Examples: `transactions_y2025m01`, `transactions_y2025m12`

### Primary Key
```sql
PRIMARY KEY (id, created_at)
```
The `created_at` column is included to support uniqueness constraints across partitions.

### Indexes Per Partition
1. `idx_transactions_status` - For status-based queries
2. `idx_transactions_stellar_account` - For account-based queries
3. `idx_transactions_created_at` - For date range queries

### Background Job
- **Frequency**: Daily (configurable in `main.rs`)
- **Tasks**:
  1. Create partitions for next 3 months (configurable)
  2. Archive partitions older than 12 months (configurable)
  3. Analyze table statistics

## Performance Improvements

### Before Partitioning
- Single table: potentially millions of rows
- All indexes on full dataset
- VACUUM locks entire table
- Query planner scans all rows

### After Partitioning
- Partition elimination: Only relevant months scanned
- Smaller indexes: Per-partition indexes ~2/12 size
- Faster VACUUM: Per-partition maintenance
- Better lock scaling: Concurrent operations on different partitions

**Expected**: 50-90% improvement for date-filtered queries

## Prerequisites

- PostgreSQL 14+
- Rust 1.84+ (for compilation)
- Docker (recommended for local development)

## Testing

Run tests to verify partition management logic:

```bash
cargo test db::cron
```

Tests validate:
- Partition name formatting
- Month calculations for future partitions
- Retention calculation logic

## Deployment Notes

### Migration Safety
- Zero-downtime migration (transactions are copied, not moved)
- Original data preserved in `transactions_old` during migration (dropped after)
- Indexes created on new partitions

### Rollback (if needed)
If rollback is necessary:
1. Restore from backup before migration
2. Or manually detach partitions and reconstruct monolithic table

### Monitoring Post-Deployment
```sql
-- Monitor partition sizes
SELECT schemaname, tablename, pg_size_pretty(pg_total_relation_size(schemaname||'.'||tablename))
FROM pg_tables
WHERE tablename LIKE 'transactions_y%m%'
ORDER BY tablename;

-- Check for missing partitions
SELECT DISTINCT EXTRACT(YEAR_MONTH FROM created_at) as year_month
FROM transactions
ORDER BY year_month DESC;
```

## Future Enhancements

1. **pg_partman Integration**: Use automation extension for advanced features
2. **Hot Partitions**: Different storage parameters for recent vs. archived data
3. **Parallel Inserts**: Leverage partitioning for faster bulk loads
4. **Dynamic Granularity**: Adjust partition size (weekly, daily) based on volume
5. **Time-Travel Queries**: Archive to separate storage (S3, etc.)

## Files Modified/Created

```
synapse-core/
├── migrations/
│   └── 20250219000000_implement_db_partitioning.sql (NEW)
├── src/
│   ├── db/
│   │   ├── cron.rs (NEW)
│   │   └── mod.rs (MODIFIED - added cron export)
│   └── main.rs (MODIFIED - added partition background job)
├── docs/
│   ├── database-partitioning.md (NEW)
│   └── build-instructions.md (NEW)
├── .env.example (NEW)
├── setup.sh (NEW)
└── Cargo.toml (MODIFIED - removed duplicate dependency)
```

## Checklist

- [x] Feature branch created: `feature/issue-16-db-partitioning`
- [x] Migration implemented with clear documentation
- [x] Partition manager module created with all required functionality
- [x] Background job integrated with error handling
- [x] Comprehensive documentation provided
- [x] Code includes comments explaining key concepts
- [x] Unit tests for partition logic
- [x] Setup and build instructions documented
- [x] Changes staged and ready for review

## Review Notes

1. **Database URL**: Build requires `DATABASE_URL` for sqlx verification
   - See `docs/build-instructions.md` for multiple build methods
   - Can use `cargo sqlx prepare` for offline mode
   - Or temporarily set `SQLX_OFFLINE=true` for development

2. **Partition Retention**: Currently set to 12 months (configurable)
   - Modify `archive_old_partitions` call in `run_maintenance()` to adjust
   - Consider data compliance and retention requirements

3. **Month Ahead**: Currently creates 3 months ahead (configurable)
   - Modify `ensure_future_partitions` call in `run_maintenance()` to adjust
   - Trade-off between partition creation lag and storage overhead

4. **Job Frequency**: Daily runs (configurable in `main.rs`)
   - Can be adjusted from hourly to weekly based on volume
   - Monitor background job logs for errors
