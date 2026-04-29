#!/bin/bash
# Migration Safety Checker for Blue-Green Deployments
# Ensures migrations are compatible with running old and new versions simultaneously

set -euo pipefail

RED='\033[0;31m'
YELLOW='\033[1;33m'
GREEN='\033[0;32m'
NC='\033[0m' # No Color

ERRORS=0
WARNINGS=0
MIGRATIONS_DIR="${1:-migrations}"

echo "🔍 Checking migration safety for blue-green deployments..."
echo "Migration directory: $MIGRATIONS_DIR"
echo ""

# Function to check a single migration file
check_migration() {
    local file="$1"
    local filename=$(basename "$file")
    
    # Skip down migrations
    if [[ "$filename" == *.down.sql ]]; then
        return 0
    fi
    
    echo "Checking: $filename"
    
    local content=$(cat "$file")
    local file_errors=0
    
    # Rule 1: Check for NOT NULL columns without DEFAULT
    if echo "$content" | grep -iE 'ADD COLUMN.*NOT NULL' | grep -ivE 'DEFAULT|GENERATED'; then
        echo -e "${RED}❌ ERROR: NOT NULL column added without DEFAULT${NC}"
        echo "   File: $filename"
        echo "   Issue: Adding NOT NULL columns breaks old app versions that don't know about the column"
        echo "   Solution: Add a DEFAULT value or make the column nullable initially"
        echo ""
        ((ERRORS++))
        ((file_errors++))
    fi
    
    # Rule 2: Check for column renames (RENAME COLUMN)
    if echo "$content" | grep -iE 'RENAME COLUMN'; then
        echo -e "${RED}❌ ERROR: Column rename detected${NC}"
        echo "   File: $filename"
        echo "   Issue: Renaming columns breaks old app versions immediately"
        echo "   Solution: Use add+migrate+drop pattern:"
        echo "     1. Add new column with same data"
        echo "     2. Deploy app that writes to both columns"
        echo "     3. Backfill data"
        echo "     4. Deploy app that reads from new column"
        echo "     5. Drop old column in separate migration"
        echo ""
        ((ERRORS++))
        ((file_errors++))
    fi
    
    # Rule 3: Check for table drops without deprecation
    if echo "$content" | grep -iE '^[[:space:]]*DROP TABLE' | grep -ivE 'IF EXISTS.*_deprecated|_old|_backup'; then
        echo -e "${RED}❌ ERROR: Table drop without deprecation period${NC}"
        echo "   File: $filename"
        echo "   Issue: Dropping tables breaks old app versions immediately"
        echo "   Solution: Add deprecation period:"
        echo "     1. Stop writing to table in app code"
        echo "     2. Wait for full deployment cycle"
        echo "     3. Drop table in separate migration"
        echo ""
        ((ERRORS++))
        ((file_errors++))
    fi
    
    # Rule 4: Check for column drops without deprecation
    if echo "$content" | grep -iE 'DROP COLUMN' | grep -ivE 'IF EXISTS'; then
        echo -e "${YELLOW}⚠️  WARNING: Column drop detected${NC}"
        echo "   File: $filename"
        echo "   Issue: Dropping columns may break old app versions"
        echo "   Recommendation: Ensure app code no longer references this column"
        echo ""
        ((WARNINGS++))
    fi
    
    # Rule 5: Check for constraint additions that might fail on existing data
    if echo "$content" | grep -iE 'ADD CONSTRAINT.*CHECK|ADD CONSTRAINT.*UNIQUE' | grep -ivE 'NOT VALID'; then
        echo -e "${YELLOW}⚠️  WARNING: Constraint added without NOT VALID${NC}"
        echo "   File: $filename"
        echo "   Issue: Adding constraints can lock tables and fail on existing data"
        echo "   Recommendation: Use NOT VALID, then VALIDATE CONSTRAINT in separate transaction"
        echo ""
        ((WARNINGS++))
    fi
    
    # Rule 6: Check for type changes
    if echo "$content" | grep -iE 'ALTER COLUMN.*TYPE'; then
        echo -e "${RED}❌ ERROR: Column type change detected${NC}"
        echo "   File: $filename"
        echo "   Issue: Type changes can break old app versions and lock tables"
        echo "   Solution: Use add+migrate+drop pattern with new column"
        echo ""
        ((ERRORS++))
        ((file_errors++))
    fi
    
    # Rule 7: Check for index creation without CONCURRENTLY
    if echo "$content" | grep -iE '^[[:space:]]*CREATE.*INDEX' | grep -ivE 'CONCURRENTLY|IF NOT EXISTS'; then
        echo -e "${YELLOW}⚠️  WARNING: Index created without CONCURRENTLY${NC}"
        echo "   File: $filename"
        echo "   Issue: Non-concurrent index creation locks the table"
        echo "   Recommendation: Use CREATE INDEX CONCURRENTLY"
        echo ""
        ((WARNINGS++))
    fi
    
    # Rule 8: Check for foreign key additions without NOT VALID
    if echo "$content" | grep -iE 'ADD CONSTRAINT.*FOREIGN KEY|ADD FOREIGN KEY' | grep -ivE 'NOT VALID'; then
        echo -e "${YELLOW}⚠️  WARNING: Foreign key added without NOT VALID${NC}"
        echo "   File: $filename"
        echo "   Issue: Adding foreign keys can lock tables"
        echo "   Recommendation: Use NOT VALID, then VALIDATE CONSTRAINT separately"
        echo ""
        ((WARNINGS++))
    fi
    
    # Rule 9: Check for enum modifications
    if echo "$content" | grep -iE 'ALTER TYPE.*ADD VALUE'; then
        echo -e "${YELLOW}⚠️  WARNING: Enum value addition detected${NC}"
        echo "   File: $filename"
        echo "   Issue: Old app versions won't recognize new enum values"
        echo "   Recommendation: Ensure old app handles unknown enum values gracefully"
        echo ""
        ((WARNINGS++))
    fi
    
    # Rule 10: Check for required columns on existing tables
    if echo "$content" | grep -iE 'ALTER TABLE.*ADD COLUMN' | grep -ivE 'IF NOT EXISTS'; then
        if echo "$content" | grep -iE 'ALTER TABLE' | grep -v 'CREATE TABLE'; then
            if ! echo "$content" | grep -iE 'DEFAULT|NULL'; then
                echo -e "${YELLOW}⚠️  WARNING: Column added to existing table${NC}"
                echo "   File: $filename"
                echo "   Issue: Verify this is safe for blue-green deployment"
                echo "   Recommendation: Ensure column is nullable or has a default"
                echo ""
                ((WARNINGS++))
            fi
        fi
    fi
    
    if [ $file_errors -eq 0 ]; then
        echo -e "${GREEN}✓ Safe${NC}"
    fi
    echo ""
}

# Check if migrations directory exists
if [ ! -d "$MIGRATIONS_DIR" ]; then
    echo -e "${RED}❌ ERROR: Migrations directory not found: $MIGRATIONS_DIR${NC}"
    exit 1
fi

# Find all .sql migration files (excluding .down.sql)
migration_files=$(find "$MIGRATIONS_DIR" -name "*.sql" ! -name "*.down.sql" | sort)

if [ -z "$migration_files" ]; then
    echo -e "${YELLOW}⚠️  No migration files found${NC}"
    exit 0
fi

# Check each migration
for file in $migration_files; do
    check_migration "$file"
done

# Summary
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "Summary:"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

if [ $ERRORS -eq 0 ] && [ $WARNINGS -eq 0 ]; then
    echo -e "${GREEN}✓ All migrations are safe for blue-green deployment${NC}"
    exit 0
elif [ $ERRORS -eq 0 ]; then
    echo -e "${YELLOW}⚠️  $WARNINGS warning(s) found${NC}"
    echo "Warnings indicate potential issues but won't block the build."
    echo "Review warnings and ensure they're acceptable for your deployment."
    exit 0
else
    echo -e "${RED}❌ $ERRORS error(s) and $WARNINGS warning(s) found${NC}"
    echo ""
    echo "Migrations must be fixed before merging."
    echo "See docs/migration-safety.md for safe migration patterns."
    exit 1
fi
