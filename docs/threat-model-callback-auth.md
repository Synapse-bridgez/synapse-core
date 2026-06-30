# Threat Model: Callback Authentication & Signature Verification

## Overview

This document describes the security properties and threat mitigations for the callback endpoint authentication and webhook signature verification scheme.

## Endpoints Protected

- `POST /callback` — Fiat deposit notification from Stellar Anchor Platform
- `POST /callback/transaction` — Alternative endpoint for the same callback
- `POST /webhook` — General webhook delivery endpoint
- `POST /admin/*` — Administrative operations
- `POST /graphql` — GraphQL endpoint
- `GET /export` — Data export endpoint

## Authentication Layers

### 1. Callback / Webhook Endpoints: API Key + HMAC-SHA256

**Flow:**
1. Client provides `X-API-Key` header (checked against tenant keys in database)
2. Client provides `X-Webhook-Timestamp` (Unix seconds at sending time)
3. Client provides `X-Webhook-Signature` (hex-encoded HMAC-SHA256)
4. Server verifies the HMAC over `{timestamp}.{body_hex}` using all valid secrets (current + grace-period previous)
5. Server enforces timestamp is within ±5 minutes of now (replay window)

**Threats Mitigated:**

- **Unauthorized Caller**: API key requirement ensures only registered tenants can send callbacks
- **Payload Tampering**: HMAC-SHA256 over raw request body ensures integrity; signature is computed before JSON schema validation
- **Replay Attacks**: Timestamp window prevents old valid signatures from being replayed indefinitely
- **Secret Compromise**: Rotation grace period allows secrets to be rotated without immediately breaking in-flight requests

### 2. Admin Endpoints: Bearer Token

**Flow:**
1. Client provides `Authorization: Bearer <token>` header
2. Server performs constant-time comparison against valid admin keys (current + grace-period previous)
3. If no `SecretsStore` is available, falls back to `ADMIN_API_KEY` environment variable
4. Fails closed: if `ADMIN_API_KEY` is not set and no `SecretsStore` is configured, the request is rejected

**Threats Mitigated:**

- **Unauthorized Admin Access**: Bearer token requirement ensures only authorized operators can access admin endpoints
- **Timing-Based Key Guessing**: Constant-time comparison (using `subtle::ConstantTimeComparison`) prevents attackers from inferring correct tokens through response timing
- **Hardcoded Credentials**: No default fallback key; deployments must explicitly set `ADMIN_API_KEY` or configure Vault

## Signing Scheme Details

### Payload Construction

The signed payload is constructed as:

```
{timestamp}.{body_hex}
```

Where:
- `timestamp` is the Unix epoch seconds when the request was sent
- `body_hex` is the raw request body encoded as a hex string

**Example:**
```
1718918654.7b22737465...
```

This ensures:
1. The signature covers the exact bytes received (not re-parsed JSON)
2. The timestamp is bound to the payload, preventing timestamp replacement attacks
3. The format is unambiguous (unlikely to be confused with other signed data formats)

### HMAC Algorithm

- **Hash Function**: SHA-256
- **Key**: The current or grace-period previous webhook secret from `SecretsStore::valid_webhook_secrets()`
- **Output Encoding**: Hex-encoded 256-bit hash (64 ASCII characters)

### Comparison

The provided signature is decoded from hex and compared to the computed signature using constant-time comparison (`subtle::ConstantTimeComparison::ct_eq`).

- **Time**: O(1) with respect to signature correctness
- **Leakage**: No information about which byte caused a mismatch is leaked

## Replay Attack Prevention

### Timestamp Window

- **Duration**: ±5 minutes (300 seconds) from server time
- **Justification**: Provides reasonable tolerance for clock skew and network latency without being unnecessarily long

### Clock Skew Tolerance

Callers should:
1. Synchronize time with NTP or similar
2. Send timestamp close to server's current time
3. Expect requests older than 5 minutes to be rejected

### Forwarded/Stored Signatures

If a signature is recorded and replayed later (even with the same timestamp), it will be rejected because:
- The timestamp is outside the window
- Or the secret has been rotated and is no longer valid (after grace period expires)

## Secret Rotation

### Grace Period

- **Duration**: 5 minutes (300 seconds)
- **Behavior**: During the grace period, both the current secret and the previous secret are valid for signature verification

### Rotation Process

1. New secret is generated in Vault (or environment)
2. `SecretsManager::start_refresh_task()` polls Vault every 5 minutes
3. On rotation, old secret becomes "previous" and is valid for 5 minutes
4. After 5 minutes, only the new secret is valid

**Consequence**: Clients have a 5-minute window to update their signing key after rotation is detected.

## Deployment Considerations

### Required Environment Variables

- `ANCHOR_WEBHOOK_SECRET` — Secret for webhook signature verification (from Vault or env)
- `ADMIN_API_KEY` — Secret for admin endpoint bearer token (from Vault or env or unset → fail)

### Optional (Vault)

- `VAULT_ADDR` — Address of Vault instance (default: `http://127.0.0.1:8200`)
- `VAULT_ROLE_ID` — AppRole role ID for authentication
- `VAULT_SECRET_ID` — AppRole secret ID for authentication
- `VAULT_AUTH_MOUNT` — Path to AppRole auth (default: `auth/approle`)
- `VAULT_KV_MOUNT` — Path to KV v2 mount (default: `secret`)

### Failure Modes

| Scenario | Behavior |
|---|---|
| Missing `ANCHOR_WEBHOOK_SECRET` | Service fails to start |
| Missing `ADMIN_API_KEY` | Admin routes return 401 for all requests |
| Vault unreachable | Refresh task logs error; existing secrets remain valid |
| Clock skew > 5 min | Webhooks rejected with 401 |
| Invalid signature | Webhook rejected with 401 |

## Known Limitations

1. **Client Time Dependency**: Clients must have reasonably accurate time (±5 minutes). Clocks severely out of sync will cause rejected requests.

2. **Single Secret Leak**: If a secret is compromised, an attacker can forge valid signatures until the rotation completes and the grace period expires (max 10 minutes).

3. **No Nonce**: The scheme uses timestamps but not per-request nonces. This is acceptable because:
   - Timestamps are unique per 5-minute window (enough for practical replay prevention)
   - The cost of storing per-request nonces would be significant

4. **Raw Body Requirement**: The signature must be computed over the raw request body received by the server. This means:
   - Proxies/load balancers must not transform the body
   - Clients must not send the body twice (e.g., due to retries with modified bodies)

## Testing & Verification

The implementation includes:

- **Unit tests** for HMAC computation and verification
- **Unit tests** for timestamp validation
- **Unit tests** for constant-time comparison
- **Integration tests** verifying:
  - Missing signature → 401
  - Invalid signature → 401
  - Expired timestamp → 401
  - Valid signature → 201/200
  - Grace-period secret still accepted → 200
  - Unauthenticated admin request → 401
  - Valid admin token → 200

## References

- [OWASP: Timing Attacks](https://owasp.org/www-community/attacks/Timing_attack)
- [RFC 2104: HMAC](https://tools.ietf.org/html/rfc2104)
- [Tokio: Security Considerations](https://tokio.rs/)
- [Subtle Crate: Constant Time Comparison](https://docs.rs/subtle/)
