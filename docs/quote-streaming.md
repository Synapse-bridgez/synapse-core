# Quote streaming

`GET /swap/quotes/ws` upgrades to WebSocket. `GET /swap/quotes/events`
returns SSE (`text/event-stream`). Supply the same `X-API-Key` or
`Authorization: Bearer …` header as tenant REST requests. Browser WebSocket
and native EventSource clients cannot set these headers: use a fetch-based SSE
client or a trusted integration proxy. Credentials in query strings are not accepted.

The `pairs` query parameter is a URL-encoded JSON array, for example
`[{"source":"USDC","destination":"XLM"}]`. An empty array subscribes to no
pairs. Asset identifiers are case-sensitive and may include an issuer or network
identifier; source and destination must differ.

WebSocket accepts these JSON messages:

```json
{"type":"subscribe","pairs":[{"source":"USDC","destination":"XLM"}]}
{"type":"unsubscribe","pairs":[{"source":"USDC","destination":"XLM"}]}
{"type":"ping"}
```

Subscribe adds pairs, unsubscribe removes them. The server acknowledges with
`{"type":"subscribed","pairs":[...]}`. Already queued events may arrive after
an unsubscribe acknowledgement. Invalid messages return `{"type":"error",
"problem":{...}}` using the shared problem-details taxonomy. Frames/messages
are limited to 16 KiB. Send ping messages or WebSocket ping frames before the
idle timeout. SSE subscriptions change by closing the old stream and opening a
new stream with the desired pairs. Closing unsubscribes and releases its slot.

Both transports send objects containing `cursor`, `sequence`, `kind` and
`quote`. `kind` is `created`, `updated` or `expired`. Quote fields are `id`,
`pair`, decimal-string `input_amount` and `output_amount`, and UTC `expires_at`.
SSE additionally names events `quote_created`, `quote_updated`, `quote_expired`
and sets the SSE `id` to the cursor.

Sequences strictly increase within a tenant; pair filters can legitimately skip
numbers. They do not represent a contiguous pair-specific sequence. Resume
with the last received `cursor` query parameter, or SSE `Last-Event-ID` (which
takes precedence). Replay and live registration happen under the same broker
lock so a concurrent publisher cannot fall between them. Changing WebSocket
subscriptions affects future publications; resume uses a fresh connection.

History is bounded per tenant. Invalid cursor syntax returns validation errors;
an unavailable cursor, process restart, or replay exceeding the connection
buffer returns HTTP 409 `SWAP_STALE_QUOTE`. Reconnect without a cursor and
request fresh pricing from the pricing integration. A fresh stream receives
future events, not an implicit cache snapshot. Trusted producers can read
`QuoteBroker::snapshot` for their ordinary quote responses. History and quotes
are process-local; use sticky routing or a shared external producer/broker for
multiple replicas. Restart invalidates all cursors. Keep pair filters constant
when reconnecting if the application relies on receiving all filtered events.

Each connection has an independent bounded channel. Publication uses
`try_send`, disconnecting full/closed subscribers without awaiting network
writes. WebSocket sends have deadlines; SSE uses a separate bounded producer
task so auth/idle checks continue even when the HTTP body stops being polled.
Dropping the body aborts that task. Both transports revalidate credentials in
storage periodically, including old-key grace expiry and tenant deactivation.
Checks fail closed on database failure. Authentication checks and shutdown can
take at most the configured send deadline beyond their interval. SSE idle time
means time without quote events, and idle consumers should reconnect. Keepalive
comments do not extend that deadline.

## Integration and configuration

Use one shared `QuoteBroker` instance in the router's `Extension`. Pricing
integrations must call `publish(tenant_id, quote)` after computing or revising
a quote: this atomically writes the cache and publishes created/updated events.
It validates positive decimal amounts, pair identity and future expiry. Tenant
identity must come from the authenticated pricing context. There is no public
publication endpoint and the Phase 1 repository has no price provider.
Expired quotes are removed and emitted once by `expire(Utc::now())`; the server
maintenance task should call this every second and call `disconnect_all()` on
shutdown. Clients must still enforce the quote's exact expiry before execution.

`QuoteStreamConfig::from_env()` supports these positive integer settings:

| Variable | Default | Meaning |
| --- | ---: | --- |
| QUOTE_STREAM_MAX_CONNECTIONS | 1024 | Shared WS/SSE connection cap |
| QUOTE_STREAM_MAX_PAIRS | 32 | Pairs per connection |
| QUOTE_STREAM_BUFFER | 128 | Events per connection |
| QUOTE_STREAM_HISTORY | 1024 | Retained events per tenant |
| QUOTE_STREAM_MAX_QUOTES | 10000 | Live quote cache cap across tenants |
| QUOTE_STREAM_MAX_TENANTS | 1000 | Tenant histories retained per process |
| QUOTE_STREAM_IDLE_SECONDS | 300 | Idle deadline |
| QUOTE_STREAM_AUTH_SECONDS | 15 | Credential revalidation interval |
| QUOTE_STREAM_SEND_SECONDS | 5 | Network send / auth lookup deadline |

Invalid/zero environment values use defaults. Tenant histories remain allocated
for process lifetime to preserve cursor semantics; size the tenant cap for the
deployment. New tenants/quotes/connections fail with `SWAP_LIMIT_EXCEEDED`
instead of growing memory without bounds. Cached quote IDs are scoped to a
tenant, and updating an ID cannot change its pair.

Unit tests cover lifecycle and expiry, isolated tenant delivery, replay gaps,
restart, slow-consumer shedding, subscription limits, shutdown cleanup and
bounded fan-out to 512 subscribers. Tests are included but were not executed,
as requested in task.txt.
