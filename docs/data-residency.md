# Data residency and regional routing

- Each tenant has a `region` (`tenants.region`). `swap::residency::RegionRouter::resolve` runs at ingress and returns a `RegionContext` (region + datastore); all swap writes (orders, quotes, fills, events, audit) use that context's datastore.
- **Fail closed:** missing/unknown region -> error; a region served by another deployment -> `CrossRegion` rejection; never defaults to a primary.
- Config: `SWAP_LOCAL_REGION`, `SWAP_REGION_DATABASES=eu-1=postgres://...,us-1=...`.
- Background jobs (partition maintenance, outbox relay, reconciliation) iterate `job_regions()` - the local region only.
- **Degraded region:** `set_health(region, Degraded)` rejects new requests for that region's tenants (never served cross-region). In-flight swaps are not migrated; they stay in their region and resume from persisted state/outbox on recovery.
- Out of scope: active-active writes, cross-region analytics, per-end-user residency.
