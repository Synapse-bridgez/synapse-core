# Outbound Webhook Signing (#1520)

Every outbound swap webhook carries `X-Synapse-Signature`:

```
t=<unix seconds>,id=<delivery id>,v1=<hex hmac-sha256>[,v1=<hex hmac-sha256>]
```

## Canonicalization

Signed string = `"{t}.{id}." ++ raw_body_bytes`, HMAC-SHA256 keyed by the tenant
signing secret, hex-encoded. Sign/verify the **exact bytes on the wire** - never
re-serialize the JSON.

## Verification

1. Parse `t`, `id`, and all `v1` values.
2. Reject if `|now - t| > 300s` (replay window; also the tolerated clock skew).
3. Recompute the HMAC for each secret you hold; accept if any `v1` matches
   (constant-time compare).
4. Optionally de-duplicate on `id` within the window.

Reference implementations: `sdks/python/synapse_webhook_verify.py` and
`synapse_core::crypto::webhook_signing::verify` (Rust).

## Secret lifecycle

- Secrets are `whsec_` + 32 random bytes (hex), shown once at creation.
- On rotation the old secret stays active for a grace period; during it each
  delivery carries one `v1` per active secret, so receivers holding either
  secret verify and in-flight retries are not dropped.
- Secrets are never logged; `SigningSecrets`' `Debug` output is redacted.
- Storage and rotation endpoints (encrypted at rest, per tenant) are not part of
  this change; `SigningSecrets` provides the generate/rotate/active primitives
  they build on.
