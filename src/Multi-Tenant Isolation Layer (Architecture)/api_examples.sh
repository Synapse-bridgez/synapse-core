#!/bin/bash

# API Examples for Synapse Core Multi-Tenant System
# Make executable: chmod +x api_examples.sh

BASE_URL="http://localhost:3000"
TENANT1_KEY="demo_api_key_anchor_platform_001"
TENANT2_KEY="test_api_key_partner_002"

echo "=== Synapse Core API Examples ==="
echo ""

# Create Transaction - Tenant 1
echo "1. Creating transaction for Tenant 1..."
RESPONSE=$(curl -s -X POST "$BASE_URL/api/transactions" \
  -H "X-API-Key: $TENANT1_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "external_id": "demo_tx_001",
    "amount": "100.50",
    "asset_code": "USDC",
    "memo": "Test transaction for Tenant 1"
  }')
echo "$RESPONSE" | jq '.'
TENANT1_TX_ID=$(echo "$RESPONSE" | jq -r '.transaction.transaction_id')
echo ""

# Create Transaction - Tenant 2
echo "2. Creating transaction for Tenant 2..."
RESPONSE=$(curl -s -X POST "$BASE_URL/api/transactions" \
  -H "X-API-Key: $TENANT2_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "external_id": "partner_tx_001",
    "amount": "250.00",
    "asset_code": "USDC",
    "memo": "Test transaction for Tenant 2"
  }')
echo "$RESPONSE" | jq '.'
TENANT2_TX_ID=$(echo "$RESPONSE" | jq -r '.transaction.transaction_id')
echo ""

# List Transactions - Tenant 1
echo "3. Listing transactions for Tenant 1 (should only see their own)..."
curl -s "$BASE_URL/api/transactions" \
  -H "X-API-Key: $TENANT1_KEY" | jq '.'
echo ""

# List Transactions - Tenant 2
echo "4. Listing transactions for Tenant 2 (should only see their own)..."
curl -s "$BASE_URL/api/transactions" \
  -H "X-API-Key: $TENANT2_KEY" | jq '.'
echo ""

# Get Specific Transaction - Tenant 1
if [ ! -z "$TENANT1_TX_ID" ] && [ "$TENANT1_TX_ID" != "null" ]; then
  echo "5. Getting specific transaction for Tenant 1..."
  curl -s "$BASE_URL/api/transactions/$TENANT1_TX_ID" \
    -H "X-API-Key: $TENANT1_KEY" | jq '.'
  echo ""
fi

# Test Cross-Tenant Access (Should Fail)
if [ ! -z "$TENANT1_TX_ID" ] && [ "$TENANT1_TX_ID" != "null" ]; then
  echo "6. Testing tenant isolation - Tenant 2 trying to access Tenant 1's transaction..."
  echo "   (This should return 404 Not Found)"
  curl -s "$BASE_URL/api/transactions/$TENANT1_TX_ID" \
    -H "X-API-Key: $TENANT2_KEY" | jq '.'
  echo ""
fi

# Update Transaction Status
if [ ! -z "$TENANT1_TX_ID" ] && [ "$TENANT1_TX_ID" != "null" ]; then
  echo "7. Updating transaction status for Tenant 1..."
  curl -s -X PUT "$BASE_URL/api/transactions/$TENANT1_TX_ID" \
    -H "X-API-Key: $TENANT1_KEY" \
    -H "Content-Type: application/json" \
    -d '{
      "status": "completed",
      "stellar_transaction_id": "stellar_abc123xyz"
    }' | jq '.'
  echo ""
fi

# Test Webhook
echo "8. Sending webhook for Tenant 1..."
curl -s -X POST "$BASE_URL/api/webhook" \
  -H "X-API-Key: $TENANT1_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "event_type": "transaction.completed",
    "transaction_id": "tx_12345",
    "data": {
      "status": "completed",
      "amount": "100.50"
    }
  }' | jq '.'
echo ""

# Test Inactive Tenant (Should Fail)
echo "9. Testing inactive tenant access (should return 401 Unauthorized)..."
curl -s "$BASE_URL/api/transactions" \
  -H "X-API-Key: inactive_api_key_003" | jq '.'
echo ""

# Test with Tenant ID Header
echo "10. Testing with X-Tenant-ID header..."
curl -s "$BASE_URL/api/transactions" \
  -H "X-Tenant-ID: 11111111-1111-1111-1111-111111111111" \
  -H "X-API-Key: $TENANT1_KEY" | jq '.'
echo ""

# Pagination Example
echo "11. Testing pagination (limit=2, offset=0)..."
curl -s "$BASE_URL/api/transactions?limit=2&offset=0" \
  -H "X-API-Key: $TENANT1_KEY" | jq '.'
echo ""

echo "=== All tests completed ==="
echo ""
echo "Summary:"
echo "- Tenant 1 Transaction ID: $TENANT1_TX_ID"
echo "- Tenant 2 Transaction ID: $TENANT2_TX_ID"
echo ""
echo "To clean up, you can delete the transactions:"
if [ ! -z "$TENANT1_TX_ID" ] && [ "$TENANT1_TX_ID" != "null" ]; then
  echo "curl -X DELETE $BASE_URL/api/transactions/$TENANT1_TX_ID -H 'X-API-Key: $TENANT1_KEY'"
fi
if [ ! -z "$TENANT2_TX_ID" ] && [ "$TENANT2_TX_ID" != "null" ]; then
  echo "curl -X DELETE $BASE_URL/api/transactions/$TENANT2_TX_ID -H 'X-API-Key: $TENANT2_KEY'"
fi
