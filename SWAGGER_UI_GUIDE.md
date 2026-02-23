# Swagger UI Access Guide

## Quick Start

After implementing issue #23 (Automated OpenAPI/Swagger Documentation), the API now provides interactive documentation.

### Starting the Server

```bash
cargo run
```

You should see the log message:
```
Swagger UI available at http://localhost:8080/swagger-ui/
```

### Accessing the Documentation

1. **Interactive Swagger UI**
   - URL: `http://localhost:8080/swagger-ui/`
   - Features:
     - Browse all endpoints
     - View request/response schemas
     - Try endpoints directly from the browser
     - See HTTP status codes and descriptions

2. **OpenAPI Specification (JSON)**
   - URL: `http://localhost:8080/api-docs/openapi.json`
   - Use this to:
     - Import into Postman
     - Import into Insomnia
     - Generate client code
     - Use with other OpenAPI tools

### Available Endpoints Documentation

All endpoints are documented with:
- Full endpoint descriptions
- Request parameters and body schemas
- Response status codes and schemas
- Field descriptions and types

#### Health Check
- **Path**: `GET /health`
- **Description**: Service health status with database connectivity check
- **Responses**: 200 (healthy) or 503 (unhealthy)

#### Settlements
- **List**: `GET /settlements` - Get paginated list of settlements
  - Query parameters: `limit`, `offset`
  - Response: Array of settlements

- **Get One**: `GET /settlements/{id}` - Get specific settlement
  - Path parameter: Settlement ID (UUID)
  - Responses: 200, 404, 500

#### Transactions
- **Get One**: `GET /transactions/{id}` - Get specific transaction
  - Path parameter: Transaction ID (UUID)
  - Returns full transaction details including amount and status

#### Webhooks
- **Handle**: `POST /webhook` - Process webhook callbacks
  - Request body: Webhook payload with id and anchor_transaction_id
  - Response: Success/failure message
  - Idempotency middleware applied

### Using the Swagger UI

1. **Browse Endpoints**
   - Each endpoint is listed by tag (Health, Settlements, Transactions, Webhooks)
   - Click on any endpoint to expand details

2. **View Schemas**
   - Scroll down to "Schemas" section to see all data models
   - Includes field descriptions and types
   - Shows relationships between types

3. **Test Endpoints**
   - Click "Try it out" on any endpoint
   - Fill in parameters
   - Click "Execute"
   - View the response and HTTP status code

4. **Copy Request Information**
   - Each endpoint shows curl command equivalent
   - Copy-paste for use in scripts or other tools

### Important Notes

- The Swagger UI makes actual requests to your running server
- Test data must exist in the database to get responses
- Authorization headers are not currently configured (can be added in future)
- All responses show exact schema with all optional fields

### Troubleshooting

**Swagger UI not loading?**
- Ensure the server is running (`cargo run`)
- Check the port (default 8080)
- Verify database is connected (check health endpoint)

**Endpoints returning 404?**
- Verify you're using correct UUID format for path parameters
- Check database has test data
- Review the error response for details

**CORS Issues?**
- If accessing from different origin, CORS middleware may need configuration
- Currently set up for local development

### Integration with Other Tools

**Postman**
1. Go to Swagger UI
2. Click the dropdown menu
3. Select "Download OpenAPI spec"
4. Import into Postman via URL: `http://localhost:8080/api-docs/openapi.json`

**Insomnia**
1. Create new request
2. Import from URL: `http://localhost:8080/api-docs/openapi.json`
3. All endpoints auto-populate with documentation

**API Client Generation**
Use tools like OpenAPI Generator:
```bash
openapi-generator-cli generate \
  -i http://localhost:8080/api-docs/openapi.json \
  -g typescript-fetch \
  -o ./generated-client
```

### Documentation Best Practices

- Descriptions are automatically generated from code documentation comments
- Keep handler documentation updated as API changes
- Schema descriptions help frontend developers understand data structures
- Status codes tell clients what to expect in different scenarios

## Next Steps

- The feature branch is ready for Pull Request against `develop`
- All endpoints are documented and compile successfully
- Swagger UI provides complete interactive API documentation
- No manual documentation updates needed for future changes
