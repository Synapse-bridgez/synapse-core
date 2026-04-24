# Webhook Signature Verification

## Overview

This implementation provides secure webhook signature verification using **versioned HMAC signatures**. Outgoing webhooks include cryptographic signatures that prevent spoofing attacks and include timestamps to prevent replay attacks.

## Security Features

1. **Versioned Signatures**: Support for multiple signing algorithms (v1: HMAC-SHA256, v2: HMAC-SHA512 prepared)
2. **Timestamp Inclusion**: Every signature includes a timestamp to prevent replay attacks
3. **HMAC Security**: Industry-standard cryptographic hash function
4. **Constant-time comparison**: Verification uses constant-time comparison to prevent timing attacks

## Signature Format

### Header Format

Outgoing webhooks include the following headers:

- `X-Webhook-Signature`: The versioned signature in format `v1=<hex_value>`
- `X-Webhook-Timestamp`: ISO 8601 formatted timestamp when the webhook was sent
- `X-Webhook-Event`: Event type (e.g., `transaction.completed`)
- `Content-Type`: `application/json`

Example:
```
X-Webhook-Signature: v1=a1b2c3d4e5f6...
X-Webhook-Timestamp: 2025-01-15T10:30:00Z
X-Webhook-Event: transaction.completed
```

### Signature Verification Algorithm

#### v1 (HMAC-SHA256)

To verify a v1 signature:

1. Extract the timestamp from `X-Webhook-Timestamp` header
2. Get the raw JSON body as a string
3. Concatenate: `signed_content = timestamp + "." + body`
4. Compute: `computed_signature = HMAC-SHA256(secret_key, signed_content)`
5. Convert to hex: `computed_hex = hex(computed_signature)`
6. Compare: `X-Webhook-Signature == "v1=" + computed_hex`

**Critical**: Use constant-time comparison to prevent timing attacks.

#### v2 (HMAC-SHA512, Prepared Structure)

Future versions will support additional algorithms. The signature format will remain compatible:
- `X-Webhook-Signature: v2=sha512_hex_value`

## Configuration

Store your webhook secret securely:

```bash
export WEBHOOK_SECRET="your_webhook_secret_key"
```

This secret is used both for:
- **Outgoing**: Signing webhooks created by this service
- **Incoming**: Verifying webhooks received from other services

## Event Filtering

Webhook endpoints can be configured with filter rules to receive only events that match specific transaction criteria. Filters are applied before enqueuing deliveries to prevent unnecessary webhook traffic.

### Filter Rules Syntax

Filter rules are stored as JSONB in the `filter_rules` column of the `webhook_endpoints` table.

#### Supported Filters

- `asset_codes` (array of strings): Only receive events for transactions with these asset codes
- `min_amount` (string): Only receive events for transactions with amount >= this value
- `max_amount` (string): Only receive events for transactions with amount <= this value

#### Examples

```json
{
  "asset_codes": ["USD", "EUR"]
}
```

Only receives events for USD and EUR transactions.

```json
{
  "min_amount": "100.00"
}
```

Only receives events for transactions with amount >= 100.00.

```json
{
  "asset_codes": ["USD"],
  "min_amount": "50.00",
  "max_amount": "1000.00"
}
```

Only receives events for USD transactions between 50.00 and 1000.00.

#### Filter Logic

- If no `filter_rules` is set, the endpoint receives all events for its subscribed `event_types`
- Filters are AND-combined (all conditions must be met)
- String comparisons are case-sensitive
- Amount comparisons use numeric comparison after parsing as decimal

### Configuration

Filter rules can be set when creating or updating webhook endpoints via the admin API:

```bash
curl -X POST /admin/webhooks/endpoints/{id}/rate-limit \
  -H "Content-Type: application/json" \
  -d '{"max_delivery_rate": 10}'
```

Note: Filter rules are configured separately from rate limits.

## Implementation Details

### Outgoing Webhooks (Dispatched by Synapse Core)

Located in `src/services/webhook_dispatcher.rs`:

1. When a transaction reaches a terminal state, the dispatcher enqueues webhook deliveries
2. For each registered endpoint, a `WebhookDelivery` is created with the serialized payload
3. During delivery (`attempt_delivery`):
   - Extract timestamp from payload
   - Create signed content: `timestamp.body`
   - Compute HMAC-SHA256 signature
   - Format as `v1=<hex>`
   - Include in `X-Webhook-Signature` header
   - Include timestamp in `X-Webhook-Timestamp` header

### Example Payload

```json
{
  "event_type": "transaction.completed",
  "transaction_id": "123e4567-e89b-12d3-a456-426614174000",
  "timestamp": "2025-01-15T10:30:00Z",
  "data": {
    "status": "completed",
    "amount": "100.00",
    "currency": "USD"
  }
}
```

## Verification Examples

### Rust

```rust
use hmac::{Hmac, Mac};
use sha2::Sha256;

fn verify_webhook(secret: &str, timestamp: &str, body: &str, signature: &str) -> bool {
    let signed_content = format!("{}.{}", timestamp, body);
    
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(signed_content.as_bytes());
    
    let computed = hex::encode(mac.finalize().into_bytes());
    let expected = format!("v1={}", computed);
    
    // Use constant-time comparison
    signature == expected
}
```

### Python

```python
import hmac
import hashlib

def verify_webhook(secret, timestamp, body, signature):
    signed_content = f"{timestamp}.{body}"
    computed = hmac.new(
        secret.encode(),
        signed_content.encode(),
        hashlib.sha256
    ).hexdigest()
    expected = f"v1={computed}"
    
    # Use constant-time comparison
    return hmac.compare_digest(signature, expected)
```

### JavaScript/Node.js

```javascript
const crypto = require('crypto');

function verifyWebhook(secret, timestamp, body, signature) {
  const signedContent = `${timestamp}.${body}`;
  const computed = crypto
    .createHmac('sha256', secret)
    .update(signedContent)
    .digest('hex');
  const expected = `v1=${computed}`;
  
  // Use constant-time comparison
  return crypto.timingSafeEqual(
    Buffer.from(signature),
    Buffer.from(expected)
  );
}
```

### cURL Example

```bash
# Set variables
SECRET="your_webhook_secret_key"
TIMESTAMP="2025-01-15T10:30:00Z"
BODY='{"id":"txn-123","status":"completed"}'

# Compute signature
SIGNED_CONTENT="${TIMESTAMP}.${BODY}"
SIGNATURE="v1=$(echo -n "${SIGNED_CONTENT}" | openssl dgst -sha256 -hmac "${SECRET}" | awk '{print $2}')"

echo "X-Webhook-Signature: ${SIGNATURE}"
echo "X-Webhook-Timestamp: ${TIMESTAMP}"
```

## Timestamp Validation

Consumers **should** validate the timestamp to prevent replay attacks:

1. Extract `X-Webhook-Timestamp` from headers
2. Parse as ISO 8601 datetime
3. Compare to current time
4. Reject if timestamp is older than your max window (e.g., 5 minutes)

Example:
```rust
use chrono::{Duration, Utc};

fn validate_timestamp(timestamp_str: &str, max_age_secs: i64) -> bool {
    if let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(timestamp_str) {
        let now = Utc::now();
        let age = now.signed_duration_since(timestamp.with_timezone(&Utc));
        age < Duration::seconds(max_age_secs)
    } else {
        false
    }
}
```

## Testing

### Generate a test signature

```bash
SECRET="test-secret"
TIMESTAMP="2025-01-15T10:30:00Z"
BODY='{"id":"txn-123"}'
SIGNED_CONTENT="${TIMESTAMP}.${BODY}"

SIGNATURE="v1=$(echo -n "${SIGNED_CONTENT}" | openssl dgst -sha256 -hmac "${SECRET}" | awk '{print $2}')"
echo "Generated signature: ${SIGNATURE}"
```

### Test with cURL

```bash
SECRET="webhook-secret"
TIMESTAMP="2025-01-15T10:30:00Z"
BODY='{"id":"txn-123"}'
SIGNED_CONTENT="${TIMESTAMP}.${BODY}"
SIGNATURE="v1=$(echo -n "${SIGNED_CONTENT}" | openssl dgst -sha256 -hmac "${SECRET}" | awk '{print $2}')"

curl -X POST http://localhost:3000/webhook \
  -H "Content-Type: application/json" \
  -H "X-Webhook-Signature: ${SIGNATURE}" \
  -H "X-Webhook-Timestamp: ${TIMESTAMP}" \
  -H "X-Webhook-Event: transaction.completed" \
  -d "${BODY}"
```

## Migration from Unversioned Signatures

If you previously used unversioned signatures, update your verification:

**Old format:**
```
X-Webhook-Signature: sha256=<hex>
Body: raw_json_only
```

**New format:**
```
X-Webhook-Signature: v1=<hex>
X-Webhook-Timestamp: <iso8601_timestamp>
Signed Content: <timestamp>.<raw_json>
```

### Update Your Consumer

1. Extract `X-Webhook-Timestamp` header
2. Change signed content to: `timestamp + "." + body`
3. Parse version from `X-Webhook-Signature` (e.g., extract `v1` from `v1=<hex>`)
4. Update signature comparison to include timestamp

## Security Best Practices

1. **Store secrets securely**: Use environment variables or secure secret management systems
2. **Use constant-time comparison**: Prevent timing attacks by always comparing the full signature
3. **Validate timestamps**: Reject timestamped signatures outside your acceptable window
4. **Rotate secrets regularly**: Implement key rotation policies
5. **HTTPS only**: Always use HTTPS for webhook delivery and validation
6. **Monitor failures**: Log and alert on signature verification failures
7. **Rate limiting**: Implement rate limiting on webhook endpoints

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
