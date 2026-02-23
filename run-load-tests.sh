#!/bin/bash
set -e

echo "🚀 Starting Synapse Core Load Testing Suite"
echo "============================================"

# Colors for output
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Start infrastructure
echo -e "${BLUE}📦 Starting load test infrastructure...${NC}"
docker-compose -f docker-compose.load.yml up -d app

# Wait for services to be healthy
echo -e "${YELLOW}⏳ Waiting for services to be healthy (30s)...${NC}"
sleep 30

# Check if services are running
if ! docker ps | grep -q synapse-load-app; then
    echo "❌ App container is not running. Check logs:"
    docker logs synapse-load-app
    exit 1
fi

echo -e "${GREEN}✅ Services are ready${NC}"
echo ""

# Run tests
echo -e "${BLUE}🧪 Test 1/4: Sustained Load Test${NC}"
echo "Duration: ~23 minutes"
docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/callback_load.js
echo ""

echo -e "${BLUE}🧪 Test 2/4: Spike Test${NC}"
echo "Duration: ~6.5 minutes"
docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/spike_test.js
echo ""

echo -e "${BLUE}🧪 Test 3/4: Idempotency Test${NC}"
echo "Duration: ~7 minutes"
docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/idempotency_test.js
echo ""

echo -e "${BLUE}🧪 Test 4/4: Soak Test${NC}"
echo "Duration: ~40 minutes (this will take a while...)"
docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/soak_test.js
echo ""

# Cleanup
echo -e "${YELLOW}🧹 Cleaning up...${NC}"
docker-compose -f docker-compose.load.yml down -v

echo ""
echo -e "${GREEN}✅ All load tests completed!${NC}"
echo ""
echo "📊 Next steps:"
echo "1. Review the test output above"
echo "2. Document results in docs/load-test-results.md"
echo "3. Identify bottlenecks and optimization opportunities"
echo "4. Run individual tests again with: docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/<test_name>.js"
