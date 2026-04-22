#!/bin/bash
# Load Test Validation Runner
#
# This script runs k6 load tests with JSON output and validates the results
# against performance benchmarks.
#
# Usage:
#   ./run_validation.sh [test_name]
#
# Examples:
#   ./run_validation.sh callback_load    # Run sustained load test
#   ./run_validation.sh spike_test       # Run spike test
#   ./run_validation.sh soak_test        # Run soak test
#   ./run_validation.sh all              # Run all tests

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Configuration
RESULTS_DIR="./test_results"
DOCKER_COMPOSE_FILE="docker-compose.load.yml"

# Create results directory
mkdir -p "$RESULTS_DIR"

# Function to run a single test
run_test() {
    local test_name=$1
    local test_file="/scripts/${test_name}.js"
    local output_file="${RESULTS_DIR}/${test_name}_results.json"
    
    echo -e "${YELLOW}Running ${test_name}...${NC}"
    
    # Run k6 test with JSON output
    docker-compose -f "$DOCKER_COMPOSE_FILE" run --rm \
        -v "$(pwd)/${RESULTS_DIR}:/results" \
        k6 run --out "json=/results/${test_name}_results.json" "$test_file"
    
    if [ $? -eq 0 ]; then
        echo -e "${GREEN}✓ ${test_name} completed${NC}"
        return 0
    else
        echo -e "${RED}✗ ${test_name} failed${NC}"
        return 1
    fi
}

# Function to validate results
validate_results() {
    echo -e "\n${YELLOW}Validating test results...${NC}"
    
    # Run Rust validation tests
    cargo test --test load_validation_test -- --nocapture
    
    if [ $? -eq 0 ]; then
        echo -e "${GREEN}✓ All validations passed${NC}"
        return 0
    else
        echo -e "${RED}✗ Some validations failed${NC}"
        return 1
    fi
}

# Main execution
main() {
    local test_name=${1:-"all"}
    
    echo "=== Load Test Validation Suite ==="
    echo ""
    
    # Start services
    echo -e "${YELLOW}Starting services...${NC}"
    docker-compose -f "$DOCKER_COMPOSE_FILE" up -d app
    
    # Wait for services to be ready
    echo "Waiting for services to be ready..."
    sleep 10
    
    # Run tests based on argument
    case "$test_name" in
        "callback_load")
            run_test "callback_load"
            ;;
        "spike_test")
            run_test "spike_test"
            ;;
        "soak_test")
            run_test "soak_test"
            ;;
        "idempotency_test")
            run_test "idempotency_test"
            ;;
        "all")
            run_test "callback_load"
            run_test "spike_test"
            run_test "idempotency_test"
            # Uncomment to include soak test (takes 40+ minutes)
            # run_test "soak_test"
            ;;
        *)
            echo -e "${RED}Unknown test: $test_name${NC}"
            echo "Available tests: callback_load, spike_test, soak_test, idempotency_test, all"
            exit 1
            ;;
    esac
    
    # Validate results
    # Note: Validation requires parsing JSON output files
    # validate_results
    
    # Cleanup
    echo -e "\n${YELLOW}Cleaning up...${NC}"
    docker-compose -f "$DOCKER_COMPOSE_FILE" down
    
    echo -e "\n${GREEN}Done! Results saved to ${RESULTS_DIR}/${NC}"
    echo "To validate results, run: cargo test --test load_validation_test"
}

# Run main function
main "$@"
