# WebSocket Graceful Shutdown

## Overview

The WebSocket module supports real-time event delivery while the application is running and clean connection draining when the process shuts down. Application shutdown is coordinated by the Axum server in `src/main.rs`: SIGTERM and SIGINT start readiness draining, then the server waits for the drain window before exiting.

For WebSocket clients, graceful shutdown means the server stops admitting new upgrade requests, lets active streams finish in-flight work where possible, releases connection-pool permits, and expects clients to reconnect and resync.

## Shutdown Sequence

1. The process receives SIGTERM or SIGINT.
2. The server marks readiness as draining.
3. Load balancers should stop routing new WebSocket upgrade requests to the instance.
4. WebSocket handlers should reject new upgrades once draining is visible.
5. Active handlers finish their current send or receive operation.
6. Active handlers send a normal close frame when a shutdown signal is available.
7. Each handler exits and drops its `ConnectionPermit`.
8. Clients reconnect to a healthy instance and use the resync protocol from `docs/websocket-pagination.md`.

## Server Responsibilities

WebSocket handlers should follow these rules:

- Check shared readiness or drain state before acquiring a `ConnectionPermit`.
- Keep the permit for the full lifetime of the WebSocket stream.
- Release the permit by allowing it to drop when the stream exits.
- Select on shutdown notifications in long-running send and receive loops.
- Prefer normal close frames for planned shutdowns.
- Avoid logging bearer tokens, raw client messages, or tenant payload data during drain handling.

## Client Responsibilities

Clients should treat shutdown closes as recoverable:

```javascript
ws.onclose = () => {
  reconnectWithBackoff();
};

function onOpen() {
  ws.send(JSON.stringify({
    type: "resync",
    limit: 50
  }));
}
```

Clients should use bounded exponential backoff with jitter. After reconnecting, clients should request a resync so missed transaction updates are recovered from the database instead of relying on messages buffered by the closing instance.

## Security Considerations

- Shutdown must not bypass WebSocket authentication.
- Tenant authorization must still be enforced for any final events sent before close.
- Close reasons must be client-safe and should not include internal errors, tokens, tenant IDs from unrelated contexts, or database details.
- Drain logs should identify operational state, connection counts, and request IDs where available, but should not include sensitive payloads.
- New upgrade rejection during drain should use the same public error style as other admission failures.

## Performance Considerations

- Shutdown loops should be bounded by the application drain window.
- Send buffers must remain bounded so slow clients cannot delay process termination indefinitely.
- Permit release must be deterministic; leaked permits make active connection counts inaccurate.
- Health checks should continue to mark stale connections unhealthy so dead sockets are closed during drain.
- Reconnect guidance should include backoff to avoid a client thundering herd when many connections move to healthy instances.

## Edge Cases

| Edge case | Expected behavior |
|-----------|-------------------|
| New upgrade arrives after drain starts | Reject before acquiring a connection permit |
| Client is slow or not reading | Drop or close according to bounded buffer policy |
| Client disconnects during drain | Handler exits and drops its permit |
| Final event send fails | Log a sanitized operational message and close |
| Process receives shutdown without prior admin drain | `main.rs` starts drain before server shutdown |
| Client misses events during drain | Client reconnects and sends a `resync` request |

## Validation Checklist

- Run `cargo test` after documentation or handler changes.
- Verify connection-pool tests still prove permits are released on drop.
- Verify health-check tests still prove stale connections can be marked unhealthy.
- Confirm any future handler implementation rejects upgrades during readiness drain.
- Confirm any future handler implementation sends only client-safe close reasons.
