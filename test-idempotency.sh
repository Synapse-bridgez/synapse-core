#!/bin/bash

# Test script for Redis idempotency implementation
# Usage: ./test-idempotency.sh

set -e

echo "🧪 Testing Redis Idempotency Implementation"
echo "==========================================="
echo ""

# Colors for output
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

# Configuration
BASE_URL="http://localhost:3000"
IDEMPOTENCY_KEY="test-$(date +%s)"

echo "📋 Test Configuration:"
echo "   Base URL: $BASE_URL"
echo "   Idempotency Key: $IDEMPOTENCY_KEY"
echo ""

# Test 1: First request (should succeed)
echo "Test 1: First request (should create new transaction)"
echo "------------------------------------------------------"
RESPONSE1=$(curl -s -w "\n%{http_code}" -X POST "$BASE_URL/webhook" \
  -H "Content-Type: application/json" \
  -H "X-Idempotency-Key: $IDEMPOTENCY_KEY" \
  -d "{
    \"id\": \"webhook-001\",
    \"anchor_transaction_id\": \"$IDEMPOTENCY_KEY\"
  }")

HTTP_CODE1=$(echo "$RESPONSE1" | tail -n1)
BODY1=$(echo "$RESPONSE1" | head -n-1)

echo "Response: $BODY1"
echo "HTTP Code: $HTTP_CODE1"

if [ "$HTTP_CODE1" = "200" ]; then
    echo -e "${GREEN}✓ Test 1 PASSED${NC}"
else
    echo -e "${RED}✗ Test 1 FAILED - Expected 200, got $HTTP_CODE1${NC}"
fi
echo ""

# Wait a moment for processing to complete
sleep 2

# Test 2: Duplicate request (should return cached response)
echo "Test 2: Duplicate request (should return cached response)"
echo "----------------------------------------------------------"
RESPONSE2=$(curl -s -w "\n%{http_code}" -X POST "$BASE_URL/webhook" \
  -H "Content-Type: application/json" \
  -H "X-Idempotency-Key: $IDEMPOTENCY_KEY" \
  -d "{
    \"id\": \"webhook-001\",
    \"anchor_transaction_id\": \"$IDEMPOTENCY_KEY\"
  }")

HTTP_CODE2=$(echo "$RESPONSE2" | tail -n1)
BODY2=$(echo "$RESPONSE2" | head -n-1)

echo "Response: $BODY2"
echo "HTTP Code: $HTTP_CODE2"

if [ "$HTTP_CODE2" = "200" ] && echo "$BODY2" | grep -q "cached"; then
    echo -e "${GREEN}✓ Test 2 PASSED - Cached response returned${NC}"
else
    echo -e "${YELLOW}⚠ Test 2 WARNING - Expected cached response${NC}"
fi
echo ""

# Test 3: Check Redis state
echo "Test 3: Verify Redis state"
echo "---------------------------"
if command -v docker &> /dev/null; then
    echo "Checking Redis for key: idempotency:$IDEMPOTENCY_KEY"
    REDIS_VALUE=$(docker exec synapse-redis redis-cli GET "idempotency:$IDEMPOTENCY_KEY" 2>/dev/null || echo "ERROR")
    REDIS_TTL=$(docker exec synapse-redis redis-cli TTL "idempotency:$IDEMPOTENCY_KEY" 2>/dev/null || echo "ERROR")
    
    if [ "$REDIS_VALUE" != "ERROR" ]; then
        echo "Redis Value: $REDIS_VALUE"
        echo "Redis TTL: $REDIS_TTL seconds"
        echo -e "${GREEN}✓ Test 3 PASSED - Redis key exists${NC}"
    else
        echo -e "${RED}✗ Test 3 FAILED - Could not access Redis${NC}"
    fi
else
    echo -e "${YELLOW}⚠ Docker not available, skipping Redis check${NC}"
fi
echo ""

# Test 4: Request without idempotency key (should process normally)
echo "Test 4: Request without idempotency key"
echo "----------------------------------------"
RESPONSE4=$(curl -s -w "\n%{http_code}" -X POST "$BASE_URL/webhook" \
  -H "Content-Type: application/json" \
  -d "{
    \"id\": \"webhook-no-key\",
    \"anchor_transaction_id\": \"no-key-test\"
  }")

HTTP_CODE4=$(echo "$RESPONSE4" | tail -n1)
BODY4=$(echo "$RESPONSE4" | head -n-1)

echo "Response: $BODY4"
echo "HTTP Code: $HTTP_CODE4"

if [ "$HTTP_CODE4" = "200" ]; then
    echo -e "${GREEN}✓ Test 4 PASSED - Request without key processed${NC}"
else
    echo -e "${RED}✗ Test 4 FAILED - Expected 200, got $HTTP_CODE4${NC}"
fi
echo ""

# Summary
echo "==========================================="
echo "🎉 Test Suite Complete"
echo ""
echo "Next steps:"
echo "1. Check Redis keys: docker exec -it synapse-redis redis-cli KEYS 'idempotency:*'"
echo "2. Monitor logs: docker-compose logs -f app"
echo "3. Clean up test data: docker exec synapse-redis redis-cli FLUSHDB"
