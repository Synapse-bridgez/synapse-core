#!/bin/bash

# Test script for POST /callback/transaction endpoint
# Usage: ./test_webhook.sh [base_url]
# Default base_url: http://localhost:3000

BASE_URL="${1:-http://localhost:3000}"

echo "Testing POST /callback/transaction endpoint at $BASE_URL"
echo "=============================================="
echo ""

# Test 1: Valid payload
echo "Test 1: Valid deposit callback"
curl -X POST "$BASE_URL/callback/transaction" \
  -H "Content-Type: application/json" \
  -d '{
    "id": "anchor-tx-12345",
    "amount_in": "100.50",
    "stellar_account": "GABCDEFGHIJKLMNOPQRSTUVWXYZ234567890ABCDEFGHIJKLMNOPQR",
    "asset_code": "USD"
  }' \
  -w "\nHTTP Status: %{http_code}\n" \
  -s
echo ""
echo "Expected: 201 Created with transaction_id"
echo ""

# Test 2: Invalid amount (zero)
echo "Test 2: Invalid amount (zero)"
curl -X POST "$BASE_URL/callback/transaction" \
  -H "Content-Type: application/json" \
  -d '{
    "id": "anchor-tx-12346",
    "amount_in": "0",
    "stellar_account": "GABCDEFGHIJKLMNOPQRSTUVWXYZ234567890ABCDEFGHIJKLMNOPQR",
    "asset_code": "USD"
  }' \
  -w "\nHTTP Status: %{http_code}\n" \
  -s
echo ""
echo "Expected: 400 Bad Request - Amount must be greater than 0"
echo ""

# Test 3: Invalid Stellar account (too short)
echo "Test 3: Invalid Stellar account (too short)"
curl -X POST "$BASE_URL/callback/transaction" \
  -H "Content-Type: application/json" \
  -d '{
    "id": "anchor-tx-12347",
    "amount_in": "50.00",
    "stellar_account": "GABCD",
    "asset_code": "USD"
  }' \
  -w "\nHTTP Status: %{http_code}\n" \
  -s
echo ""
echo "Expected: 400 Bad Request - Stellar account must be 56 characters"
echo ""

# Test 4: Invalid Stellar account (wrong prefix)
echo "Test 4: Invalid Stellar account (wrong prefix)"
curl -X POST "$BASE_URL/callback/transaction" \
  -H "Content-Type: application/json" \
  -d '{
    "id": "anchor-tx-12348",
    "amount_in": "75.25",
    "stellar_account": "XABCDEFGHIJKLMNOPQRSTUVWXYZ234567890ABCDEFGHIJKLMNOPQR",
    "asset_code": "USD"
  }' \
  -w "\nHTTP Status: %{http_code}\n" \
  -s
echo ""
echo "Expected: 400 Bad Request - Stellar account must start with 'G'"
echo ""

# Test 5: Empty asset code
echo "Test 5: Empty asset code"
curl -X POST "$BASE_URL/callback/transaction" \
  -H "Content-Type: application/json" \
  -d '{
    "id": "anchor-tx-12349",
    "amount_in": "100.00",
    "stellar_account": "GABCDEFGHIJKLMNOPQRSTUVWXYZ234567890ABCDEFGHIJKLMNOPQR",
    "asset_code": ""
  }' \
  -w "\nHTTP Status: %{http_code}\n" \
  -s
echo ""
echo "Expected: 400 Bad Request - Asset code must be between 1 and 12 characters"
echo ""

# Test 6: Large amount
echo "Test 6: Large amount deposit"
curl -X POST "$BASE_URL/callback/transaction" \
  -H "Content-Type: application/json" \
  -d '{
    "id": "anchor-tx-12350",
    "amount_in": "999999.99",
    "stellar_account": "GABCDEFGHIJKLMNOPQRSTUVWXYZ234567890ABCDEFGHIJKLMNOPQR",
    "asset_code": "USDC"
  }' \
  -w "\nHTTP Status: %{http_code}\n" \
  -s
echo ""
echo "Expected: 201 Created with transaction_id"
echo ""

echo "=============================================="
echo "Testing complete!"
