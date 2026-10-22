# Swap receipts and tenant statements

`src/swap/receipts.rs`; tables `swap_receipts` (append-only, one per order).

- Receipt is sealed once at settlement; `content_hash` is SHA-256 over all fields (`verify()` detects tampering). Partial/compensated orders carry `refunded_amount`.
- Statements render CSV / JSON / PDF from one field list (`FIELDS`), streaming per receipt. Filters: asset, direction, status.
- **Timezone: all period boundaries are UTC, `[from, to)`.** Range is capped at 366 days; callers must also rate limit generation.
- Downloads use HMAC-SHA256 signed, expiring links bound to the tenant id and canonical query.
