# Migration Safety for Blue-Green Deployments

## Overview

During blue-green deployments, both old and new application versions run simultaneously. This means database migrations must be compatible with **both versions** of the application code. Unsafe migrations can cause:

- Application crashes in the old version
- Data corruption
- Deployment rollback failures
- Service downtime

This document outlines safe migration patterns enforced by our CI pipeline.

## Automated Safety Checks

All migrations are automatically checked by `scripts/check-migration-safety.sh` in CI. The checker enforces these rules:

### ❌ Blocking Errors

These patterns will **fail CI** and block PR merges:

1. **NOT NULL columns without DEFAULT**
2. **Column renames**
3. **Table drops without deprecation**
4. **Column type changes**

### ⚠️ Warnings

These patterns generate warnings but don't block CI:

1. Column drops
2. Constraints without NOT VALID
3. Indexes without CONCURRENTLY
4. Foreign keys without NOT VALID
5. Enum modifications

## Safe Migration Patterns

### ✅ Adding Nullable Columns

**Safe:** Old app ignores new columns, new app uses them.

```sql
-- ✅ SAFE: Nullable column
ALTER TABLE transactions
ADD COLUMN IF NOT EXISTS memo TEXT;
```

### ✅ Adding Columns with Defaults

**Safe:** Old app ignores column, new app gets default value for existing rows.

```sql
-- ✅ SAFE: Column with default
ALTER TABLE transactions
ADD COLUMN IF NOT EXISTS status VARCHAR(20) DEFAULT 'pending';
```

### ❌ Adding NOT NULL Columns Without Defaults

**Unsafe:** Old app tries to insert rows without the new column, violating NOT NULL constraint.

```sql
-- ❌ UNSAFE: NOT NULL without default
ALTER TABLE transactions
ADD COLUMN status VARCHAR(20) NOT NULL;
```

**Fix:** Add a default value:

```sql
-- ✅ SAFE: NOT NULL with default
ALTER TABLE transactions
ADD COLUMN status VARCHAR(20) NOT NULL DEFAULT 'pending';
```

### ✅ Renaming Columns (Multi-Step Pattern)

**Never use `RENAME COLUMN`** - it breaks old app versions immediately.

Instead, use the **add+migrate+drop** pattern:

#### Step 1: Add new column (Migration 1)

```sql
-- Add new column with same data type
ALTER TABLE users
ADD COLUMN IF NOT EXISTS username VARCHAR(255);

-- Backfill existing data
UPDATE users SET username = user_name WHERE username IS NULL;
```

#### Step 2: Deploy app that writes to both columns

Update application code to write to both `user_name` and `username`.

#### Step 3: Deploy app that reads from new column

Update application code to read from `username` instead of `user_name`.

#### Step 4: Drop old column (Migration 2, after full deployment)

```sql
-- Safe to drop after all instances use new column
ALTER TABLE users
DROP COLUMN IF EXISTS user_name;
```

### ✅ Dropping Tables (Deprecation Pattern)

**Never drop tables immediately** - old app versions may still query them.

#### Step 1: Stop writing to table

Update application code to stop writing to the table.

#### Step 2: Wait for full deployment

Ensure all old app instances are replaced.

#### Step 3: Drop table (separate migration)

```sql
-- Safe after deprecation period
DROP TABLE IF EXISTS old_table_name;
```

### ✅ Changing Column Types (Multi-Step Pattern)

**Never use `ALTER COLUMN TYPE`** - it can break old app versions and lock tables.

#### Step 1: Add new column with new type

```sql
ALTER TABLE transactions
ADD COLUMN amount_cents BIGINT;

-- Backfill data
UPDATE transactions 
SET amount_cents = (amount * 100)::BIGINT 
WHERE amount_cents IS NULL;
```

#### Step 2: Deploy app using new column

Update application code to use `amount_cents`.

#### Step 3: Drop old column

```sql
ALTER TABLE transactions
DROP COLUMN IF EXISTS amount;
```

### ✅ Adding Indexes Safely

Use `CONCURRENTLY` to avoid locking tables:

```sql
-- ✅ SAFE: Non-blocking index creation
CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_transactions_status 
ON transactions(status);
```

Without `CONCURRENTLY`, index creation locks the table and blocks writes.

### ✅ Adding Constraints Safely

Use `NOT VALID` to avoid full table scans and locks:

```sql
-- Step 1: Add constraint without validation
ALTER TABLE transactions
ADD CONSTRAINT check_amount_positive 
CHECK (amount > 0) NOT VALID;

-- Step 2: Validate in separate transaction (can be done later)
ALTER TABLE transactions
VALIDATE CONSTRAINT check_amount_positive;
```

### ✅ Adding Foreign Keys Safely

Similar to constraints, use `NOT VALID`:

```sql
-- Step 1: Add FK without validation
ALTER TABLE transactions
ADD CONSTRAINT fk_settlement 
FOREIGN KEY (settlement_id) REFERENCES settlements(id) NOT VALID;

-- Step 2: Validate separately
ALTER TABLE transactions
VALIDATE CONSTRAINT fk_settlement;
```

### ✅ Modifying Enums

Adding enum values is generally safe, but old app versions won't recognize them:

```sql
-- ✅ SAFE: Adding enum value
ALTER TYPE transaction_status ADD VALUE IF NOT EXISTS 'disputed';
```

**Important:** Ensure old app code handles unknown enum values gracefully (e.g., treats them as a default state).

## Migration Checklist

Before submitting a PR with migrations:

- [ ] Run `./scripts/check-migration-safety.sh` locally
- [ ] All blocking errors are resolved
- [ ] Warnings are reviewed and acceptable
- [ ] Multi-step migrations are documented in PR description
- [ ] Deployment order is clear (if multiple migrations)
- [ ] Rollback plan is documented

## Testing Migrations

### Local Testing

```bash
# Check migration safety
./scripts/check-migration-safety.sh

# Test migration forward
sqlx migrate run

# Test migration backward (if .down.sql exists)
sqlx migrate revert
```

### CI Testing

The CI pipeline automatically:

1. Runs migration safety checks
2. Applies migrations to test database
3. Runs application tests against migrated schema

## Common Scenarios

### Scenario: Adding a Required Field

**Wrong approach:**
```sql
ALTER TABLE users ADD COLUMN email VARCHAR(255) NOT NULL;
```

**Right approach:**
```sql
-- Migration 1: Add nullable column
ALTER TABLE users ADD COLUMN email VARCHAR(255);

-- Migration 2 (after app deployment): Make it required
ALTER TABLE users ALTER COLUMN email SET NOT NULL;
```

### Scenario: Splitting a Column

**Example:** Split `full_name` into `first_name` and `last_name`

```sql
-- Migration 1: Add new columns
ALTER TABLE users 
ADD COLUMN first_name VARCHAR(255),
ADD COLUMN last_name VARCHAR(255);

-- Backfill data
UPDATE users 
SET 
  first_name = split_part(full_name, ' ', 1),
  last_name = split_part(full_name, ' ', 2)
WHERE first_name IS NULL;

-- Deploy app that uses new columns

-- Migration 2: Drop old column
ALTER TABLE users DROP COLUMN full_name;
```

### Scenario: Making a Column Required

```sql
-- Migration 1: Add column as nullable with default
ALTER TABLE transactions 
ADD COLUMN status VARCHAR(20) DEFAULT 'pending';

-- Backfill existing rows
UPDATE transactions SET status = 'pending' WHERE status IS NULL;

-- Migration 2 (after backfill): Make it NOT NULL
ALTER TABLE transactions 
ALTER COLUMN status SET NOT NULL;
```

## Rollback Considerations

Always provide `.down.sql` migrations for rollback:

```sql
-- 20260429000000_add_user_email.sql
ALTER TABLE users ADD COLUMN email VARCHAR(255);

-- 20260429000000_add_user_email.down.sql
ALTER TABLE users DROP COLUMN IF EXISTS email;
```

**Important:** Down migrations should also follow safety rules. Dropping columns immediately may break the app version being rolled back to.

## Resources

- [PostgreSQL ALTER TABLE Documentation](https://www.postgresql.org/docs/current/sql-altertable.html)
- [Zero-Downtime Migrations](https://www.braintreepayments.com/blog/safe-operations-for-high-volume-postgresql/)
- [Strong Migrations (Ruby, but principles apply)](https://github.com/ankane/strong_migrations)

## Questions?

If you're unsure whether a migration is safe:

1. Run `./scripts/check-migration-safety.sh`
2. Review this document
3. Ask in #engineering-database channel
4. Consider breaking the migration into multiple steps

**Remember:** It's better to deploy in multiple steps than to cause downtime.
