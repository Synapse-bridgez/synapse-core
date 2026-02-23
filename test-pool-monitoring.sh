#!/bin/bash

echo "Testing Database Connection Pool Monitoring"
echo "==========================================="
echo ""

# Test health endpoint
echo "1. Testing /health endpoint for pool statistics..."
curl -s http://localhost:3000/health | jq '.'

echo ""
echo "2. Pool statistics should include:"
echo "   - active_connections"
echo "   - idle_connections"
echo "   - max_connections"
echo "   - usage_percent"
echo ""
echo "3. Check application logs for pool monitoring messages (every 30s)"
echo "   - DEBUG: Normal pool status"
echo "   - WARN: High pool usage (>80%)"
