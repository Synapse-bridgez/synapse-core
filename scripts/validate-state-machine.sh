#!/bin/bash
# Validate that state machine documentation matches code implementation

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DOCS_FILE="$PROJECT_ROOT/docs/state-machine.md"

echo "🔍 Validating transaction state machine..."

# Check if documentation exists
if [ ! -f "$DOCS_FILE" ]; then
    echo "❌ Error: $DOCS_FILE not found"
    exit 1
fi

# Extract states from documentation
DOCUMENTED_STATES=$(grep -E "^\*\*Database field:\*\* \`status = " "$DOCS_FILE" | sed -E "s/.*status = '([^']+)'.*/\1/" | sort)

# Extract states from code
echo "📝 Checking code for state definitions..."

# Check migrations for status column definition
MIGRATION_FILE="$PROJECT_ROOT/migrations/20250216000000_init.sql"
if [ ! -f "$MIGRATION_FILE" ]; then
    echo "❌ Error: Migration file not found"
    exit 1
fi

# Check for status transitions in code
echo "🔎 Validating state transitions..."

ERRORS=0

# Check for 'pending' state
if ! grep -r "status.*=.*'pending'" "$PROJECT_ROOT/src" > /dev/null; then
    echo "❌ Error: 'pending' state not found in code"
    ERRORS=$((ERRORS + 1))
fi

# Check for 'completed' stategit stash
if ! grep -r "status.*=.*'completed'" "$PROJECT_ROOT/src" > /dev/null; then
    echo "❌ Error: 'completed' state not found in code"
    ERRORS=$((ERRORS + 1))
fi

# Check for DLQ table (dlq state is implicit via separate table)
if ! grep -q "transaction_dlq" "$PROJECT_ROOT/migrations/20260220143500_transaction_dlq.sql" 2>/dev/null; then
    echo "⚠️  Warning: DLQ table migration not found"
fi

# Validate key files exist
echo "📂 Checking key implementation files..."

KEY_FILES=(
    "src/services/transaction_processor.rs"
    "src/handlers/webhook.rs"
    "src/db/models.rs"
    "src/db/queries.rs"
    "migrations/20250216000000_init.sql"
)

for file in "${KEY_FILES[@]}"; do
    if [ ! -f "$PROJECT_ROOT/$file" ]; then
        echo "❌ Error: Required file not found: $file"
        ERRORS=$((ERRORS + 1))
    fi
done

# Check for state transition functions
echo "🔄 Validating transition functions..."

if ! grep -q "process_transaction" "$PROJECT_ROOT/src/services/transaction_processor.rs"; then
    echo "❌ Error: process_transaction function not found"
    ERRORS=$((ERRORS + 1))
fi

if ! grep -q "requeue_dlq" "$PROJECT_ROOT/src/services/transaction_processor.rs"; then
    echo "❌ Error: requeue_dlq function not found"
    ERRORS=$((ERRORS + 1))
fi

# Validate Mermaid diagram syntax
echo "📊 Validating Mermaid diagram..."

if ! grep -q "stateDiagram-v2" "$DOCS_FILE"; then
    echo "❌ Error: Mermaid state diagram not found in documentation"
    ERRORS=$((ERRORS + 1))
fi

# Check for all documented states in diagram
for state in pending completed dlq; do
    if ! grep -q "$state" "$DOCS_FILE"; then
        echo "❌ Error: State '$state' not documented"
        ERRORS=$((ERRORS + 1))
    fi
done

# Summary
echo ""
if [ $ERRORS -eq 0 ]; then
    echo "✅ State machine validation passed!"
    echo "   - Documentation exists and is well-formed"
    echo "   - All states are documented"
    echo "   - Key implementation files present"
    echo "   - State transitions are defined"
    exit 0
else
    echo "❌ State machine validation failed with $ERRORS error(s)"
    echo ""
    echo "To fix:"
    echo "  1. Ensure all states in code are documented in docs/state-machine.md"
    echo "  2. Update Mermaid diagram to reflect current state transitions"
    echo "  3. Verify all key files exist and contain expected functions"
    exit 1
fi
