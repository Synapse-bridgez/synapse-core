#!/bin/bash
# PR Verification Script
# This script runs all CI checks locally to verify the PR is ready

set -e

echo "========================================="
echo "PR Verification Script"
echo "========================================="
echo ""

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Track results
PASSED=0
FAILED=0

# Function to run a check
run_check() {
    local name=$1
    local command=$2
    
    echo -e "${YELLOW}Running: $name${NC}"
    echo "Command: $command"
    echo ""
    
    if eval "$command"; then
        echo -e "${GREEN}✓ $name PASSED${NC}"
        echo ""
        ((PASSED++))
        return 0
    else
        echo -e "${RED}✗ $name FAILED${NC}"
        echo ""
        ((FAILED++))
        return 1
    fi
}

# 1. Format Check
run_check "Format Check" "cargo fmt --check"

# 2. Clippy Check
run_check "Clippy (Lint) Check" "cargo clippy --all-targets --all-features -- -D warnings"

# 3. Build Check
run_check "Build Check" "cargo build --all-features"

# 4. Test Check
run_check "Test Check" "cargo test --all-features"

# 5. Specific test for load validation
run_check "Load Validation Tests" "cargo test --test load_validation_test"

# Summary
echo "========================================="
echo "Summary"
echo "========================================="
echo -e "Passed: ${GREEN}$PASSED${NC}"
echo -e "Failed: ${RED}$FAILED${NC}"
echo ""

if [ $FAILED -eq 0 ]; then
    echo -e "${GREEN}✓ All checks passed! PR is ready for merge.${NC}"
    exit 0
else
    echo -e "${RED}✗ Some checks failed. Please fix the issues above.${NC}"
    exit 1
fi
