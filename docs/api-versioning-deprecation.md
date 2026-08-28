# API v1/v2 Parity Audit and v1 Deprecation Plan

Status: audit complete. Tracking issue: #1105.

## Summary

`/api/v1` and `/api/v2` are served from the **same route set** with the same handlers.
The only behavioural difference is that v1 responses additionally carry `Deprecation`
and `Sunset` headers. There is no v1-only functionality, so v1 can be sunset without
porting anything to v2.

## How versioning is wired

`src/lib.rs` builds one `core_routes` router and mounts it three times:

| Mount | Middleware | Notes |
| --- | --- | --- |
| `/api/v1/*` | `middleware::versioning::v1_version_middleware` | adds `API-Version: v1`, `Deprecation: true`, `Sunset: Fri, 31 Dec 2026 23:59:59 GMT` |
| `/api/v2/*` | `middleware::versioning::v2_version_middleware` | adds `API-Version: v2` |
| unversioned (`/*`) | `v2_version_middleware` | unversioned paths default to v2 behaviour |

`src/middleware/versioning.rs` also exposes `inject_deprecation_headers` and
`inject_api_version_header` helpers; `Accept-Version` request-header negotiation is
present in the header-injection path.

Note: `src/handlers/v1/mod.rs` and `src/handlers/v2/mod.rs` are byte-identical
re-export stubs declared in `src/handlers/mod.rs` but referenced nowhere - routing
uses `handlers::*` directly. They are dead code and can be removed as a follow-up
(out of scope for this audit).

## Parity matrix

Every route in `core_routes` (`src/lib.rs`) is reachable identically under both
prefixes. The handler column is the single implementation both versions share.

| Route | Method | Handler | v1 | v2 |
| --- | --- | --- | --- | --- |
| `/transactions/:id` | GET | `handlers::webhook::get_transaction` | ✅ | ✅ |
| `/transactions` | GET | `handlers::webhook::list_transactions_api` | ✅ | ✅ |
| `/transactions/search` | GET | `handlers::search::search_transactions_wrapper` | ✅ | ✅ |
| `/settlements` | GET | `handlers::settlements::list_settlements` | ✅ | ✅ |
| `/settlements/:id` | GET | `handlers::settlements::get_settlement` | ✅ | ✅ |
| `/callback` | POST | `handlers::webhook::callback` | ✅ | ✅ |
| `/callback/transaction` | POST | `handlers::webhook::callback` | ✅ | ✅ |
| `/webhook` | POST | `handlers::webhook::handle_webhook` | ✅ | ✅ |

Response-header differences (by design):

| Header | v1 | v2 |
| --- | --- | --- |
| `API-Version` | `v1` | `v2` |
| `Deprecation` | `true` | absent |
| `Sunset` | `Fri, 31 Dec 2026 23:59:59 GMT` | absent |

**v1-only functionality:** none.
**v2-only functionality:** none.
**Trivial gap-fills identified:** none - the versions are already at full parity.

## Consumers

| Consumer | Version coupling |
| --- | --- |
| `sdks/rust` (`SynapseClient`) | none - takes a `base_url` and appends paths; the caller chooses `/api/v1` or `/api/v2` |
| `cli/synapse-cli` | none - uses the SDK client with a configured base URL |
| External integrators | pin the version through their configured base URL |

Migration for every consumer is the same: point the base URL at `/api/v2` (or drop the
version prefix, which already defaults to v2).

## Deprecation plan and sunset timeline

The sunset date is already advertised to clients via the `Sunset` header:
**2026-12-31**. This plan formalizes the milestones around it.

| Phase | Date | Action |
| --- | --- | --- |
| Announce | on merge of this audit | Publish this document; open the v1-removal follow-up issue; notify integrators via the usual channel that v1 == v2 and v1 is deprecated. |
| Deprecation headers | already live | `Deprecation: true` + `Sunset` on every `/api/v1` response (no change needed). |
| Reminder | 2026-10-01 | Second integrator notice; add a startup log line counting `/api/v1` requests per tenant over the last 30 days so remaining v1 traffic is visible. |
| Final notice | 2026-12-01 | Last integrator notice; confirm zero (or accepted) v1 traffic. |
| Sunset | 2026-12-31 | `/api/v1` begins returning `410 Gone` (or `301` to the `/api/v2` equivalent). |
| Removal | follow-up issue, after sunset | Delete the `/api/v1` mount and the dead `handlers::v1` / `handlers::v2` stub modules. |

### Client communication milestones

- **Announce** and each **notice** go out on the integrator channel with: the sunset
  date, the fact that v1 and v2 are functionally identical, and the one-line migration
  (change the base URL).
- No code change is required of any integrator that is already version-agnostic.

## Out of scope (follow-ups)

- Removing the `/api/v1` mount and the dead `handlers::v1` / `handlers::v2` modules.
- Un-`#[ignore]`ing `tests/api_versioning_test.rs` (needs a running Postgres/Redis; it
  already asserts the v1 deprecation headers and v2's absence of them).
- `410`/`301` behaviour at sunset.
