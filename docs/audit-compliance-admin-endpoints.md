# Runbook: Audit Log Search & Compliance Report Endpoints

## What changed

`GET /admin/audit/search` and `POST`/`GET /admin/compliance/reports` are now
live. Both handlers (`src/handlers/admin/audit.rs`,
`src/handlers/admin/compliance.rs`) already existed, fully implemented and
unit-tested, but were never `mod`-declared in `src/handlers/admin/mod.rs` —
they did not compile into the running binary at all, so every request to
these paths returned a generic 404 from the router, indistinguishable from a
typo'd URL. As of this change they are declared, mounted on the admin router,
and gated behind the same `admin_auth` bearer-token check as every other
`/admin/*` endpoint.

## How to confirm this endpoint was never available before now

If you need to establish, for an audit or incident retrospective, that this
capability genuinely did not exist prior to a given date (rather than "existed
but nobody used it"):

1. **Deploy history**: check when the deploy containing this change went out
   — every request to `/admin/audit/search` or `/admin/compliance/reports`
   before that deploy timestamp received a 404, because the route did not
   exist in the router at all (not a 401/403 — those require the route to
   exist first). Application/load-balancer access logs will show `404` for
   these exact paths on any pre-deploy attempt.
2. **Git history**: `src/handlers/admin/mod.rs` had no `pub mod audit;` /
   `pub mod compliance;` line, and `src/lib.rs`'s admin router builder had no
   `.route("/admin/audit/search", ...)` / `.route("/admin/compliance/reports", ...)`
   entries, before this change. `git log -S 'pub mod audit' -- src/handlers/admin/mod.rs`
   shows exactly when (if ever ) that changed.
3. **Metrics**: `admin_audit_search_requests_total` and
   `admin_compliance_report_requests_total` (see `src/metrics.rs`) did not
   exist before this change, so there is no historical time series to check
   — their absence in your metrics backend prior to the deploy date is itself
   confirming evidence, not a gap in instrumentation.

## How to use it

### Audit log search

```
GET /admin/audit/search?actor=<name>&action=<action>&entity_type=<type>&from_date=<iso8601>&to_date=<iso8601>&limit=<1-500>&cursor=<opaque>
Authorization: Bearer <admin key>
```

- All filters are optional and combine with AND.
- Returns `{ "total": <count>, "data": [...], "next_cursor": <string|null> }`.
  Pass `next_cursor` back as `?cursor=` to page forward; a `null` cursor means
  you've reached the last page.
- Add `&export=true` to receive `text/csv` instead of JSON (same filters
  apply, no pagination — the full filtered result set streams as one file).

### Compliance reports

```
POST /admin/compliance/reports?period=daily|weekly|monthly
Authorization: Bearer <admin key>
```

Generates and persists a new compliance report covering the given period
(relative to "now" at generation time), returning the created report as JSON
(`201 Created`).

```
GET /admin/compliance/reports?period=<optional>&limit=<default 20>&offset=<default 0>
Authorization: Bearer <admin key>
```

Lists previously generated reports, optionally filtered by `period`, newest
`period_start` first.

## Monitoring

- `admin_audit_search_requests_total` — request count for the search
  endpoint. A sustained zero after this deploy most likely means the
  compliance team hasn't started using it yet, not that it's broken — check
  the endpoint manually with a valid admin key before escalating.
- `admin_compliance_report_requests_total{operation="generate"|"list"}` —
  request count by operation.
- Both endpoints run under the standard `admin_auth` middleware, so
  authentication failures show up the same way any other admin endpoint's do
  (401/429 responses, `admin_auth_lockout_triggered_total` on rate-limit
  lockout) — see the main [runbook](runbook.md#security-operations).
