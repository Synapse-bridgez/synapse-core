# Swap API Versioning and Deprecation Policy

Tracking issue: #1507.

## Scheme

Path prefix: `/swap/v{N}/...` (tenant) and `/admin/swap/v{N}/...` (operator).
All swap routes are mounted only through `swap::versioning::swap_router` /
`admin_swap_router`, so a new endpoint cannot bypass the policy.

| Request | Result |
| --- | --- |
| `/swap/v1/analytics` | served, `API-Version: v1` |
| `/swap/analytics` (unversioned) | `400 version_required`, body lists supported versions |
| `/swap/v9/analytics` (unsupported) | `404 unsupported_version`, body lists supported versions |
| WebSocket / SSE | `?version=v1` or `Sec-WebSocket-Protocol: swap.v1` via `negotiate_stream_version`; unsupported -> error listing supported versions |

Version negotiation uses the path/query/subprotocol only, never a custom header that proxies may strip.

## Deprecation registry

`swap::versioning::VERSIONS` holds each version's status. Marking a version
`Deprecated { sunset }` makes every response under it carry `Deprecation: true`
and `Sunset: <date>`.

## Compatibility rules

**Breaking** (requires a new major `/swap/v{N+1}`): removing a path, method, response code or field; renaming a field; making an optional parameter required; narrowing accepted values; changing a field's type or meaning.
**Non-breaking**: new endpoints, new optional parameters, new response fields, new enum values clients must already tolerate.

- Minimum support window: a version is supported for at least **12 months** after its successor is GA.
- Notice period: deprecation (headers + changelog) at least **180 days** before sunset.

## CI enforcement

`docs/openapi/swap.json` is the published spec. `.github/workflows/swap-openapi-compat.yml`
runs `scripts/check-openapi-breaking.py` against the base branch and fails on a breaking
change unless `info.version`'s major is bumped. The workflow also self-tests that a
simulated breaking change (removed path) is rejected.

## Tenant migration guide

1. Call `/swap/v1/...` explicitly; unversioned calls fail with `version_required`.
2. Watch for `Deprecation`/`Sunset` response headers and the `API-Version` header.
3. When a new version is announced, move to it before the sunset date; both versions run side by side during the support window.
