# Tenant Secret Rotation - Operator Guide

## Overview

Tenant webhook secrets can be rotated without any downtime to active integrations. The rotation mechanism supports a configurable **grace period** during which both the old and new secrets validate simultaneously, allowing in-flight requests and deployed consumers to migrate seamlessly.

After the grace period expires, the old secret is automatically and permanently revoked. Every rotation and revocation event is written to the immutable `audit_logs` table.

---

## Architecture

### Database Columns (added in `migrations/20260828000003_hash_tenant_secrets_rotation.sql`)

| Column | Type | Description |
| :--- | :--- | :--- |
| `webhook_secret` | `VARCHAR(255)` | The current active secret |
| `previous_webhook_secret` | `VARCHAR(255)` | The immediately preceding secret (active during grace period) |
| `previous_secret_expires_at` | `TIMESTAMPTZ` | When the previous secret is revoked |
| `secret_updated_at` | `TIMESTAMPTZ` | Timestamp of the last rotation |

### Dual-Validation Logic (`src/tenant/mod.rs`)

```rust
pub fn validate_webhook_secret(&self, candidate: &str) -> bool {
    // 1. Validate against current secret
    if self.webhook_secret == candidate { return true; }

    // 2. Validate against previous secret if grace period is still active
    if let (Some(prev), Some(exp)) = (&self.previous_webhook_secret, self.previous_secret_expires_at) {
        if prev == candidate && Utc::now() < exp { return true; }
    }
    false
}
```

---

## Rotating a Secret

### Via CLI

```bash
# Rotate with default 24h grace period (generates a new secret automatically)
synapse-core tenant rotate-secret --tenant-id <TENANT_UUID>

# Rotate with custom grace period (e.g. 1 hour = 3600 seconds)
synapse-core tenant rotate-secret \
  --tenant-id <TENANT_UUID> \
  --grace-period 3600

# Rotate with a specific new secret
synapse-core tenant rotate-secret \
  --tenant-id <TENANT_UUID> \
  --new-secret "my-new-strong-secret" \
  --grace-period 7200 \
  --admin-actor "operator_alice"
```

**Output:**
```
=== Tenant Secret Rotated Successfully ===
Tenant ID: 550e8400-e29b-41d4-a716-446655440000
New Secret: rotated_secret_...
Grace Period: 3600 seconds
Previous Secret Expires At: 2026-08-28T09:00:00Z

Both old and new secrets will validate until the expiration timestamp.
```

### Via Admin REST API

**Endpoint:** `POST /admin/tenants/:id/rotate-secret`

**Required headers:**
```http
X-Admin-Role: admin
Content-Type: application/json
```

**Request body:**
```json
{
  "grace_period_seconds": 86400,
  "new_secret": "optional-explicit-secret"
}
```

**Response (200 OK):**
```json
{
  "tenant_id": "550e8400-e29b-41d4-a716-446655440000",
  "new_secret": "rotated_secret_abc123...",
  "grace_period_seconds": 86400,
  "previous_secret_expires_at": "2026-08-29T08:00:00Z"
}
```

**Error responses:**
- `403 Forbidden` — missing or insufficient `X-Admin-Role` header
- `500 Internal Server Error` — database error or tenant not found

---

## Revoking Expired Secrets

Previous secrets past their `previous_secret_expires_at` timestamp are automatically null-cleared by the janitor command:

```bash
# Revoke all expired tenant secrets and write audit logs
synapse-core tenant revoke-expired-secrets
```

This is safe to run as a scheduled cron job (e.g. every 5 minutes in CI/CD or Kubernetes CronJob).

---

## Audit Trail

All rotation and revocation events are immutably logged to the `audit_logs` table.

### Query Rotation Events

```sql
-- List all secret rotation events for a specific tenant
SELECT action, old_val, new_val, actor, timestamp
FROM audit_logs
WHERE entity_id = '<TENANT_UUID>'
  AND entity_type = 'tenant'
  AND action IN ('secret_rotation_issued', 'secret_rotation_revoked')
ORDER BY timestamp DESC;
```

### Example Audit Log Entries

**Secret rotation issuance:**
```json
{
  "entity_type": "tenant",
  "action": "secret_rotation_issued",
  "old_val": {
    "previous_secret_hash": "initial_secret_abc",
    "previous_secret_expires_at": "2026-08-29T08:00:00Z"
  },
  "new_val": {
    "new_secret_issued": true,
    "grace_period_seconds": 86400,
    "expires_at": "2026-08-29T08:00:00Z"
  },
  "actor": "operator_alice",
  "timestamp": "2026-08-28T08:00:00Z"
}
```

**Post-grace-period revocation:**
```json
{
  "entity_type": "tenant",
  "action": "secret_rotation_revoked",
  "old_val": { "previous_secret_active": true },
  "new_val": { "previous_secret_revoked": true },
  "actor": "system_janitor",
  "timestamp": "2026-08-29T08:01:00Z"
}
```

---

## Incident Response: Suspected Credential Compromise

Use this procedure when a tenant secret may have been exposed:

1. **Immediately rotate the secret** with a short grace period (e.g. 300 seconds / 5 minutes):
   ```bash
   synapse-core tenant rotate-secret \
     --tenant-id <TENANT_UUID> \
     --grace-period 300 \
     --admin-actor "incident-responder"
   ```

2. **Notify the tenant** of the new secret and expiry deadline.

3. **Monitor audit logs** for any authentication attempts using the old secret during the grace period:
   ```sql
   SELECT * FROM audit_logs
   WHERE entity_id = '<TENANT_UUID>'
     AND entity_type = 'tenant'
   ORDER BY timestamp DESC LIMIT 20;
   ```

4. **Force-expire immediately** if needed (reduce `grace-period` to `1`).

5. After expiry, run the revocation sweep:
   ```bash
   synapse-core tenant revoke-expired-secrets
   ```

---

## Security Notes

- The CLI `rotate-secret` command requires a valid `--admin-actor` to be recorded in the audit log.
- The REST endpoint requires the `X-Admin-Role: admin` (or `superadmin`) header per session hardening standards (Issue #18).
- Secrets are stored hashed at rest as per `migrations/20260824000003_hash_tenant_secrets.sql`.
- Only one previous secret is retained — a second rotation before the first grace period expires will overwrite the previous previous secret.
