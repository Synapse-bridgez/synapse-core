# Vault runtime-outage fallback (#1336)

- Only secrets classed `LongLivedApproved` (`anchor_webhook_secret`, `admin_api_key`) may be served from the last-known-good cache; `ShortLived`/`SingleUse` classes never are. DB credentials are out of scope (pool-held).
- Hard max age from last successful Vault read: `VAULT_SECRET_FALLBACK_MAX_AGE_SECS` (default 900, clamped 360–3600). Past it, verification fails closed (`secrets_stale_rejected_total`). A stalled refresh loop is bounded the same way.
- While failing, refresh retries every `VAULT_OUTAGE_RETRY_SECS` (default 30).
- Telemetry: `secrets_cached_fallback_used_total`, `vault_refresh_failures_total`, `vault_secret_staleness_seconds`, `vault_fallback_active`, ERROR log on entering fallback/expiry.
- `/ready` reports `dependencies.vault` (`degraded_cached_fallback` / `expired`, time remaining) and `X-Degraded-Dependencies: vault` while staying 200; set `READINESS_FAIL_ON_EXPIRED_SECRETS=true` to return 503 once expired.
