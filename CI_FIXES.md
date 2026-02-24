# CI/CD Fixes Applied

## Summary
This document lists all the changes made to ensure the codebase passes CI/CD checks.

## Changes Made

### 1. Code Formatting
- Applied `cargo fmt` to format all Rust code according to rustfmt standards
- All formatting issues resolved

### 2. Migration Fixes
- **Moved `partition_utils.sql`** from `migrations/` to `docs/` directory
  - This file is not a migration but a utility script
  - sqlx requires migration files to have numeric prefixes
  
- **Removed duplicate partition migration**
  - Deleted `migrations/20260219000000_partition_transactions.sql` (duplicate)
  - Kept `migrations/20250217000000_partition_transactions.sql`
  
- **Fixed duplicate index creation**
  - Modified `20250217000000_partition_transactions.sql` to use `CREATE INDEX IF NOT EXISTS`
  - Prevents conflict with index created in init migration
  
- **Renamed migration to avoid timestamp collision**
  - Renamed `20260222000000_transaction_memo_metadata.sql` to `20260222000001_transaction_memo_metadata.sql`
  - Two migrations had the same timestamp causing primary key violation
  
- **Removed foreign key constraint in DLQ table**
  - Modified `20260220143500_transaction_dlq.sql`
  - Partitioned tables don't support foreign keys to non-unique columns
  - Added comment about application-level referential integrity

### 3. Clippy Fixes
- **Removed unused imports**:
  - `utoipa::ToSchema` from `src/db/models.rs`
  - `ENTITY_SETTLEMENT` and `TransactionDlq` from `src/db/queries.rs`
  
- **Fixed redundant field names**:
  - Changed `anchor_webhook_secret: anchor_webhook_secret` to `anchor_webhook_secret` in `src/config.rs`

### 4. Remaining Issues (To Be Fixed)
The following issues still need to be addressed:

- Deprecated function usage:
  - `base64::encode` and `base64::decode` in `src/utils/cursor.rs`
  - `chrono::DateTime::from_utc` in `src/db/cron.rs`
  - `chrono::TimeZone::ymd_opt` in `src/db/cron.rs`
  
- Unused imports in various files
- Missing fields in test fixtures
- Config struct field mismatches in tests

## Testing
- All migrations now run successfully
- Database schema is properly created
- Code formatting passes `cargo fmt --check`

## Next Steps
1. Fix remaining clippy warnings
2. Update test fixtures with new Transaction fields (memo, memo_type, metadata)
3. Update deprecated function calls to use new APIs
4. Remove unused imports
