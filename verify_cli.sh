#!/bin/bash
# Verification script for Issue #24 Admin CLI implementation

set -e

echo "=== Synapse Core Admin CLI Verification ==="
echo ""

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Check 1: Files exist
echo "1. Checking files exist..."
if [ -f "src/cli.rs" ] && [ -f "src/main.rs" ]; then
    echo -e "${GREEN}✓${NC} All required files exist"
else
    echo -e "${RED}✗${NC} Missing files"
    exit 1
fi

# Check 2: Cargo.toml has clap
echo "2. Checking Cargo.toml for clap dependency..."
if grep -q 'clap.*=.*{.*version.*=.*"4"' Cargo.toml; then
    echo -e "${GREEN}✓${NC} clap dependency added"
else
    echo -e "${RED}✗${NC} clap dependency missing"
    exit 1
fi

# Check 3: CLI module imported in main.rs
echo "3. Checking main.rs imports cli module..."
if grep -q "mod cli;" src/main.rs; then
    echo -e "${GREEN}✓${NC} cli module imported"
else
    echo -e "${RED}✗${NC} cli module not imported"
    exit 1
fi

# Check 4: All required subcommands exist
echo "4. Checking subcommands in cli.rs..."
MISSING=""
grep -q "Serve" src/cli.rs || MISSING="${MISSING}serve "
grep -q "ForceComplete" src/cli.rs || MISSING="${MISSING}tx-force-complete "
grep -q "Migrate" src/cli.rs || MISSING="${MISSING}db-migrate "
grep -q "Config" src/cli.rs || MISSING="${MISSING}config "

if [ -z "$MISSING" ]; then
    echo -e "${GREEN}✓${NC} All subcommands present"
else
    echo -e "${RED}✗${NC} Missing subcommands: $MISSING"
    exit 1
fi

# Check 5: Handler functions exist
echo "5. Checking handler functions..."
MISSING=""
grep -q "handle_tx_force_complete" src/cli.rs || MISSING="${MISSING}handle_tx_force_complete "
grep -q "handle_db_migrate" src/cli.rs || MISSING="${MISSING}handle_db_migrate "
grep -q "handle_config_validate" src/cli.rs || MISSING="${MISSING}handle_config_validate "

if [ -z "$MISSING" ]; then
    echo -e "${GREEN}✓${NC} All handler functions present"
else
    echo -e "${RED}✗${NC} Missing handlers: $MISSING"
    exit 1
fi

# Check 6: SQL query correctness
echo "6. Checking SQL query in force-complete handler..."
if grep -q "UPDATE transactions SET status = 'completed', updated_at = NOW()" src/cli.rs; then
    echo -e "${GREEN}✓${NC} SQL query correct"
else
    echo -e "${RED}✗${NC} SQL query incorrect or missing"
    exit 1
fi

# Check 7: Uses existing Config and PgPool
echo "7. Checking code reuse..."
if grep -q "config::Config::from_env" src/main.rs && \
   grep -q "db::create_pool" src/main.rs; then
    echo -e "${GREEN}✓${NC} Reuses existing Config and PgPool"
else
    echo -e "${RED}✗${NC} Not reusing existing code"
    exit 1
fi

# Check 8: Password masking
echo "8. Checking password masking..."
if grep -q "mask_password" src/cli.rs; then
    echo -e "${GREEN}✓${NC} Password masking implemented"
else
    echo -e "${YELLOW}⚠${NC} Password masking not found"
fi

# Check 9: Default behavior (serve)
echo "9. Checking default command is serve..."
if grep -q "Option<Commands>" src/cli.rs && \
   grep -q "None => serve" src/main.rs; then
    echo -e "${GREEN}✓${NC} Default command is serve"
else
    echo -e "${RED}✗${NC} Default command not properly set"
    exit 1
fi

echo ""
echo "=== Static Verification Complete ==="
echo ""
echo -e "${YELLOW}Now run compilation tests:${NC}"
echo "  cargo check"
echo "  cargo build"
echo ""
echo -e "${YELLOW}Then run functional tests:${NC}"
echo "  cargo run -- --help"
echo "  cargo run -- config"
echo "  cargo run -- db migrate"
echo ""
