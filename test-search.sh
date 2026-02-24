#!/bin/bash

# Test script for transaction search endpoint

BASE_URL="http://localhost:8080"

echo "Testing Transaction Search API"
echo "================================"
echo ""

# Test 1: Basic search without filters
echo "Test 1: Search all transactions"
curl -s "${BASE_URL}/transactions/search" | jq '.'
echo ""

# Test 2: Filter by status
echo "Test 2: Search by status=completed"
curl -s "${BASE_URL}/transactions/search?status=completed" | jq '.'
echo ""

# Test 3: Filter by asset code
echo "Test 3: Search by asset_code=USDC"
curl -s "${BASE_URL}/transactions/search?asset_code=USDC" | jq '.'
echo ""

# Test 4: Filter by amount range
echo "Test 4: Search by amount range (100-5000)"
curl -s "${BASE_URL}/transactions/search?min_amount=100&max_amount=5000" | jq '.'
echo ""

# Test 5: Filter by date range
echo "Test 5: Search by date range"
curl -s "${BASE_URL}/transactions/search?from=2025-01-01T00:00:00Z&to=2025-02-01T00:00:00Z" | jq '.'
echo ""

# Test 6: Filter by Stellar account
echo "Test 6: Search by stellar_account"
curl -s "${BASE_URL}/transactions/search?stellar_account=GABCD1234..." | jq '.'
echo ""

# Test 7: Combined filters
echo "Test 7: Combined filters (status + asset_code + amount range)"
curl -s "${BASE_URL}/transactions/search?status=completed&asset_code=USDC&min_amount=100&max_amount=5000" | jq '.'
echo ""

# Test 8: Pagination
echo "Test 8: Pagination with limit"
curl -s "${BASE_URL}/transactions/search?limit=10" | jq '.'
echo ""

echo "Tests completed!"
