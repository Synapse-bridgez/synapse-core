#!/bin/bash

# Test script for POST /callback/transaction endpoint
# Usage: ./test-callback.sh

BASE_URL="${BASE_URL:-http://localhost:3000}"

echo "Testing POST /callback/transaction endpoint..."
echo "Base URL: $BASE_URL"
echo ""

# Test 1: Valid callback
echo "Test 1: Valid callback payload"
curl -X POST "$BASE_URL/callback/transaction" \
  -H "Content-Type: application/json" \
  -d '{
    "id": "anchor-tx-12345",
    "amount_in": "100.50",
    "stellar_account": "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890ABCDEFGHIJKLMNOP",
    "asset_code": "USD",
    "callback_type": "deposit",
    "status": "completed"
  }' \
  -w "\nHTTP Status: %{http_code}\n\n"

# Test 2: Invalid amount (negative)
echo "Test 2: Invalid amount (should return 400)"
curl -X POST "$BASE_URL/callback/transaction" \
  -H "Content-Type: application/json" \
  -d '{
    "id": "anchor-tx-12346",
    "amount_in": "-50.00",
    "stellar_account": "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890ABCDEFGHIJKLMNOP",
    "asset_code": "USD"
  }' \
  -w "\nHTTP Status: %{http_code}\n\n"

# Test 3: Invalid Stellar account (too short)
echo "Test 3: Invalid Stellar account (should return 400)"
curl -X POST "$BASE_URL/callback/transaction" \
  -H "Content-Type: application/json" \
  -d '{
    "id": "anchor-tx-12347",
    "amount_in": "100.50",
    "stellar_account": "INVALID",
    "asset_code": "USD"
  }' \
  -w "\nHTTP Status: %{http_code}\n\n"

# Test 4: Invalid asset code (too long)
echo "Test 4: Invalid asset code (should return 400)"
curl -X POST "$BASE_URL/callback/transaction" \
  -H "Content-Type: application/json" \
  -d '{
    "id": "anchor-tx-12348",
    "amount_in": "100.50",
    "stellar_account": "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890ABCDEFGHIJKLMNOP",
    "asset_code": "TOOLONGASSETCODE"
  }' \
  -w "\nHTTP Status: %{http_code}\n\n"

# Test 5: Zero amount
echo "Test 5: Zero amount (should return 400)"
curl -X POST "$BASE_URL/callback/transaction" \
  -H "Content-Type: application/json" \
  -d '{
    "id": "anchor-tx-12349",
    "amount_in": "0",
    "stellar_account": "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890ABCDEFGHIJKLMNOP",
    "asset_code": "USD"
  }' \
  -w "\nHTTP Status: %{http_code}\n\n"

echo "Tests completed!"
