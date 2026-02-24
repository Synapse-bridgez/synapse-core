# Webhook Signature Verification

## Overview

This implementation provides secure webhook signature verification for Anchor Platform callbacks using HMAC-SHA256. It prevents spoofing attacks by validating the `X-Stellar-Signature` header against the request payload.

## Security Features

1. **HMAC-SHA256**: Industry-standard cryptographic hash function
2. **Constant-time comparison**: Prevents timing attacks using the `hmac` crate's built-in verification
3. **Automatic rejection**: Invalid or missing signatures return 401 Unauthorized

## Configuration

Add the webhook secret to your environment variables:

```bash
ANCHOR_WEBHOOK_SECRET=your_secret_key_here
```

This secret must match the one configured in your Anchor Platform instance.

## Implementation Details

### Components

1. **`src/handlers/auth.rs`**: Contains the `VerifiedWebhook` extractor
   - Extracts `X-Stellar-Signature` header
   - Reads request body
   - Computes HMAC-SHA256 of body
   - Performs constant-time comparison
   - Returns 401 on failure

2. **`src/handlers/webhook.rs`**: Transaction callback handler
   - Uses `VerifiedWebhook` extractor for automatic verification
   - Only processes requests with valid signatures

3. **`src/config.rs`**: Configuration management
   - Loads `ANCHOR_WEBHOOK_SECRET` from environment

### How It Works

1. Anchor Platform sends a POST request to `/callback/transaction`
2. Request includes `X-Stellar-Signature` header with hex-encoded HMAC-SHA256
3. `VerifiedWebhook` extractor:
   - Extracts the signature header
   - Reads the request body
   - Computes HMAC-SHA256(secret, body)
   - Compares computed signature with header value using constant-time comparison
4. If verification succeeds, handler processes the callback
5. If verification fails, returns 401 Unauthorized

### Signature Format

The `X-Stellar-Signature` header contains a hex-encoded HMAC-SHA256 hash:
- Algorithm: HMAC-SHA256
- Key: `ANCHOR_WEBHOOK_SECRET`
- Message: Raw request body (JSON)
- Encoding: Hexadecimal (64 characters)

## Usage Example

```rust
use crate::handlers::auth::VerifiedWebhook;

pub async fn transaction_callback(
    State(state): State<AppState>,
    VerifiedWebhook { body }: VerifiedWebhook,
) -> impl IntoResponse {
    // Body is already verified - safe to process
    let callback: TransactionCallback = serde_json::from_slice(&body)?;
    // ... process callback
}
```

## Testing

Generate a test signature:

```bash
echo -n '{"id":"123","status":"completed"}' | \
  openssl dgst -sha256 -hmac "your_secret_key" | \
  awk '{print $2}'
```

Send a test request:

```bash
SIGNATURE=$(echo -n '{"id":"123","status":"completed"}' | \
  openssl dgst -sha256 -hmac "your_secret_key" | awk '{print $2}')

curl -X POST http://localhost:3000/callback/transaction \
  -H "Content-Type: application/json" \
  -H "X-Stellar-Signature: $SIGNATURE" \
  -d '{"id":"123","status":"completed"}'
```

## Error Responses

| Status Code | Error | Description |
|-------------|-------|-------------|
| 401 | Missing X-Stellar-Signature header | Header not present in request |
| 401 | Invalid signature format | Signature is not valid hex |
| 401 | Signature verification failed | Signature doesn't match computed value |
| 400 | Failed to read request body | Body parsing error |
| 500 | Invalid webhook secret configuration | Server configuration error |

## References

- [Stellar Anchor Platform Authentication](https://developers.stellar.org/docs/anchoring-platform/admin-guide/authentication)
- [HMAC RFC 2104](https://www.rfc-editor.org/rfc/rfc2104)
- [Constant-time comparison](https://codahale.com/a-lesson-in-timing-attacks/)
