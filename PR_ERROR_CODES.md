## Structured Error Codes & Error Catalog

### Summary
Added machine-readable error codes to enable API consumers to programmatically handle specific failure scenarios.

### Changes
- Every AppError variant now has a unique stable error code (e.g., ERR_VALIDATION_001)
- JSON error responses now include the code field: `{ "error": "message", "code": "ERR_VALIDATION_001", "status": 400 }`
- Added GET /errors endpoint returning error catalog as JSON
- Created static error catalog at docs/error-catalog.md

### New Files
- `docs/error-catalog.md` - Static error catalog with all 19 error codes

### Modified Files
- `src/error.rs` - Added code() method, error code constants, ErrorCode struct
- `src/handlers/mod.rs` - Added error_catalog handler
- `src/main.rs` - Added /errors route
- `src/lib.rs` - Added /errors route

### Error Codes (19 total)
- ERR_DATABASE_001/002 - Database errors
- ERR_VALIDATION_001 - Validation errors
- ERR_NOT_FOUND_001 - Not found errors
- ERR_INTERNAL_001 - Internal errors
- ERR_BAD_REQUEST_001 - Bad request errors
- ERR_AUTH_001/002 - Authentication errors
- ERR_UNAUTHORIZED_001 - Unauthorized errors
- ERR_TRANSACTION_001-005 - Transaction errors
- ERR_WEBHOOK_001/002 - Webhook errors
- ERR_SETTLEMENT_001/002 - Settlement errors
- ERR_RATE_LIMIT_001 - Rate limiting

### Example Response
```json
{
  "error": "Validation error: invalid email",
  "code": "ERR_VALIDATION_001",
  "status": 400
}
```
