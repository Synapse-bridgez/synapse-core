# Dependency Health Graph

The authenticated `GET /admin/health/graph` endpoint returns the current
service readiness and upstream dependency observations as a JSON graph.
`GET /health/graph` serves a data-free visualization shell. Enter an admin
API key there to fetch the protected JSON graph; the key is sent in the
`Authorization: Bearer` header, cleared from the input, and not persisted.
The data endpoint uses the existing `admin_auth` middleware.

The graph declares four service dependencies:

| Node | Probe | Edge |
|---|---|---|
| Postgres | `health::PostgresChecker` | Critical |
| Redis | `health::RedisChecker` | Non-critical |
| Vault | Existing `/ready` Vault secret-freshness state | Non-critical |
| Settlement network API | `health::HorizonChecker` | Non-critical |

The settlement-network node is the existing Horizon API probe. Vault state
comes from readiness's cached-secret fallback status, so this endpoint does
not issue another Vault request. Readiness may be `not_configured` when no
Vault-backed secrets are in use; this is returned as `unknown` rather than
reported healthy.

Each node returns `status` and `own_status`. The service node additionally
returns `dependency_status` and `affected_by`: this keeps a healthy process
distinct from an unhealthy dependency that degrades its effective status.
The edge list declares the static service-to-upstream relationships, and its
`critical` flag identifies the Postgres edge. Dependency probe errors are
intentionally summarized rather than echoed, to avoid disclosing connection
details; probe latency and readiness fallback context are included where
available.

The GraphQL, auth, telemetry, and WebSocket health helpers are not represented
as separately probed nodes because they are not held as live checkers in
application state. GraphQL schema construction is a startup gate; GraphQL
resolvers and subscriptions depend on Postgres. Telemetry health helper
results are not a live exporter probe. The graph reports the shared live
upstreams instead of presenting synthetic component results as measurements.