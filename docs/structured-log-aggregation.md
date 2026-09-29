# Structured Log Aggregation

The application can forward JSON stdout logs through a Fluent Bit agent to an
existing Loki-compatible store. The agent parses the Docker log envelope while
preserving application JSON fields, including `trace_id`, `tenant_id`, and
`request_id` wherever the corresponding application event emits them.
Successful API-key authentication adds the database-verified tenant UUID to a
tracing span around downstream work. Pre-authentication, rejected, and
admin-only events may not have a tenant ID. Only low-cardinality `service` and
`environment` values are Loki labels; correlation identifiers remain
searchable JSON fields to avoid high-cardinality index growth.

Set `LOKI_HOST` to the existing Loki service hostname, then start/recreate the
Compose services with the logging overlay:

```sh
LOKI_HOST=loki.internal.example \
  docker compose -f docker-compose.yml -f docker-compose.logging.yml up -d
```

The Fluent Bit receiver is bound to loopback on TCP port 24224. The Docker
Fluentd logging driver uses asynchronous connection setup and buffers records
while the agent is unavailable. Configure the Loki endpoint to be reachable
from the Fluent Bit container; use the platform's TLS-protected internal
endpoint when logs cross an untrusted network. For non-Compose deployments,
configure the Fluent Bit forward input or native container-log input and retain
the same JSON parser/output settings.

Example LogQL searches:

```logql
{service="synapse-core", environment="production"} | json | tenant_id="tenant-123"
{service="synapse-core", environment="production"} | json | trace_id="trace-abc"
{service="synapse-core", environment="production"} | json | request_id="request-xyz"
```

The request middleware emits both the legacy `correlation_id` and
search-friendly `request_id`. Trace and verified tenant fields emitted by
instrumented tracing spans are retained in downstream JSON events. Application secrets and
connection strings are not added to the forwarding configuration.