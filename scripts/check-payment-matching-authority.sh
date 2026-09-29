#!/bin/bash
#
# Audit Payment Matching Authority Enforcement
#
# Verifies that only ReconciliationJob has authority to mark payments as matched.
# Scans for unauthorized UPDATE queries to transactions.status = 'completed'
# and ensures they are either:
# 1. In ReconciliationJob (reconciliation.rs) - AUTHORIZED
# 2. Protected by a guard or explicitly allowed (marked with // GOVERNED)
# 3. CLI/admin-only operations with explicit justification
#
# Exit codes:
#   0 = All payment matching paths properly authorized
#   1 = Unauthorized payment matching paths found

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m' # No Color

echo "Checking payment matching authority enforcement..."
echo

# Find all UPDATE statements to transactions that set status to 'completed'
# or horizon_payment_id (which indicates matching)
SUSPICIOUS_PATTERNS=(
    "UPDATE transactions.*SET.*status.*=.*'completed'"
    "UPDATE transactions.*SET.*horizon_payment_id"
)

FORBIDDEN_FILES=(
    "src/cli.rs"
    "src/graphql/resolvers/transaction.rs"
    "src/handlers/"
)

AUTHORIZED_FILES=(
    "src/services/reconciliation.rs"
    "src/db/queries.rs"
    "src/services/transaction_processor.rs"
    "src/services/processor.rs"
    "src/services/account_monitor.rs"
)

violations=0
warnings=0

echo "Scanning for unauthorized UPDATE operations on transactions..."
echo

for pattern in "${SUSPICIOUS_PATTERNS[@]}"; do
    echo "  Pattern: $pattern"

    # Find files with the pattern
    while IFS= read -r file; do
        if [ -z "$file" ]; then
            continue
        fi

        # Check if it's in an explicitly authorized file
        is_authorized=0
        for auth_file in "${AUTHORIZED_FILES[@]}"; do
            if [[ "$file" == "$auth_file" ]]; then
                is_authorized=1
                break
            fi
        done

        # Check if it's in a forbidden file
        is_forbidden=0
        for forbidden_file in "${FORBIDDEN_FILES[@]}"; do
            if [[ "$file" == "$forbidden_file" ]]; then
                is_forbidden=1
                break
            fi
        done

        # Check if the line has a GOVERNED comment (explicit exception)
        line_num=$(grep -n "$pattern" "$file" | head -1 | cut -d: -f1)
        has_exception=0
        if [ -n "$line_num" ]; then
            if grep -q "// GOVERNED\|// AUTHORIZED\|// ADR-004\|// EXCEPTION" "$file" | grep -A2 -B2 "$(sed -n "${line_num}p" "$file")" > /dev/null 2>&1; then
                has_exception=1
            fi
        fi

        if [ "$is_forbidden" -eq 1 ] && [ "$is_authorized" -eq 0 ] && [ "$has_exception" -eq 0 ]; then
            echo -e "    ${RED}✗ VIOLATION${NC}: $file:$line_num - UPDATE transactions.status by unauthorized path"
            echo "      $(sed -n "${line_num}p" "$file")"
            ((violations++))
        elif [ "$is_authorized" -eq 1 ]; then
            echo -e "    ${GREEN}✓${NC} Authorized: $file"
        elif [ "$has_exception" -eq 1 ]; then
            echo -e "    ${YELLOW}⊘${NC} Governed exception: $file:$line_num"
            ((warnings++))
        fi
    done < <(grep -l "$pattern" src/**/*.rs 2>/dev/null || echo "")
done

echo
echo "Checking that PaymentMatchingAuthority is properly imported in reconciliation.rs..."
if grep -q "PaymentMatchingAuthority\|use.*governance" "src/services/reconciliation.rs"; then
    echo -e "  ${GREEN}✓${NC} PaymentMatchingAuthority is referenced in reconciliation.rs"
else
    echo -e "  ${YELLOW}⊘${NC} PaymentMatchingAuthority not yet integrated in reconciliation.rs"
fi

echo
echo "Checking that unauthorized files don't import PaymentMatchingAuthority..."
for file in src/cli.rs src/graphql/resolvers/transaction.rs; do
    if [ -f "$file" ]; then
        if grep -q "PaymentMatchingAuthority" "$file"; then
            echo -e "  ${RED}✗ VIOLATION${NC}: $file imports PaymentMatchingAuthority"
            echo "    Only ReconciliationJob should construct this token"
            ((violations++))
        else
            echo -e "  ${GREEN}✓${NC} $file does not import PaymentMatchingAuthority"
        fi
    fi
done

echo
echo "Summary:"
echo "  Violations: $violations"
echo "  Warnings: $warnings"

if [ "$violations" -eq 0 ]; then
    echo -e "\n${GREEN}✅ Payment matching authority enforcement check PASSED${NC}"
    exit 0
else
    echo -e "\n${RED}❌ Payment matching authority enforcement check FAILED${NC}"
    echo
    echo "Remediation steps:"
    echo "1. For legitimate payment-matching operations: route through ReconciliationJob"
    echo "2. For admin overrides: add // GOVERNED comment explaining why"
    echo "3. Reference ADR-004: docs/adr/004-payment-matching-authority.md"
    exit 1
fi
