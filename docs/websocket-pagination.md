# WebSocket Pagination

## Overview

The WebSocket module provides real-time event streaming with pagination support for transaction status updates. This document describes the pagination mechanism, message protocol, and best practices for consuming real-time events.

For planned server restarts and drain behavior, see [WebSocket Graceful Shutdown](websocket-graceful-shutdown.md).

## Connection

### Establishing a WebSocket Connection

```javascript
const ws = new WebSocket('ws://localhost:3000/ws?token=your-token');

ws.onopen = () => {
  console.log('Connected to WebSocket');
};

ws.onmessage = (event) => {
  const message = JSON.parse(event.data);
  handleMessage(message);
};

ws.onerror = (error) => {
  console.error('WebSocket error:', error);
};

ws.onclose = () => {
  console.log('WebSocket closed');
};
```

### Authentication

WebSocket connections require authentication via query parameter:

```
ws://localhost:3000/ws?token=your-bearer-token
```

The token must be a valid bearer token. Invalid or missing tokens result in a 401 Unauthorized response.

## Message Protocol

### Server Messages

The server sends messages with the following structure:

```json
{
  "type": "message_type",
  "data": {}
}
```

#### Transaction Update

Sent when a transaction status changes:

```json
{
  "type": "transaction_update",
  "transaction_id": "550e8400-e29b-41d4-a716-446655440000",
  "tenant_id": "550e8400-e29b-41d4-a716-446655440001",
  "status": "completed",
  "timestamp": "2025-05-26T21:07:39.611Z",
  "message": "Transaction processed successfully"
}
```

#### Resync Response

Sent in response to a `resync` request with the latest N events:

```json
{
  "type": "resync",
  "events": [
    {
      "id": "550e8400-e29b-41d4-a716-446655440000",
      "tenant_id": "550e8400-e29b-41d4-a716-446655440001",
      "status": "completed",
      "created_at": "2025-05-26T21:07:39.611Z"
    }
  ]
}
```

#### Messages Dropped

Sent when the server drops messages due to client being slow:

```json
{
  "type": "messages_dropped",
  "count": 5
}
```

### Client Messages

Clients can send the following message types:

#### Resync Request

Request the latest N events from the database:

```json
{
  "type": "resync",
  "limit": 20
}
```

**Parameters:**

| Parameter | Type | Required | Default | Max | Description |
|-----------|------|----------|---------|-----|-------------|
| `limit` | Integer | No | 20 | 100 | Number of events to retrieve |

## Pagination

### Resync Mechanism

The resync mechanism allows clients to catch up with missed events:

1. **Client sends resync request** with desired limit
2. **Server queries database** for latest N transactions
3. **Server sends resync response** with events
4. **Client processes events** and updates local state

### Limits

- **Default limit**: 20 events
- **Maximum limit**: 100 events
- **Minimum limit**: 1 event

Requests exceeding the maximum limit are clamped to 100.

### Example: Catching Up After Reconnection

```javascript
class TransactionMonitor {
  constructor(wsUrl) {
    this.wsUrl = wsUrl;
    this.transactions = new Map();
    this.connect();
  }

  connect() {
    this.ws = new WebSocket(this.wsUrl);
    
    this.ws.onopen = () => {
      // Request latest 50 events on connection
      this.resync(50);
    };

    this.ws.onmessage = (event) => {
      const message = JSON.parse(event.data);
      
      if (message.type === 'resync') {
        // Process historical events
        message.events.forEach(tx => {
          this.transactions.set(tx.id, tx);
        });
      } else if (message.type === 'transaction_update') {
        // Process real-time update
        this.transactions.set(message.transaction_id, {
          id: message.transaction_id,
          status: message.status,
          timestamp: message.timestamp
        });
      }
    };
  }

  resync(limit) {
    this.ws.send(JSON.stringify({
      type: 'resync',
      limit: Math.min(limit, 100)
    }));
  }
}
```

## Backpressure Handling

### Message Buffering

The server maintains a broadcast channel with a limited buffer:

- **Buffer size**: 1000 messages per client
- **Overflow behavior**: Older messages are dropped
- **Notification**: Client receives `messages_dropped` notification

### Handling Dropped Messages

When receiving a `messages_dropped` notification:

```javascript
ws.onmessage = (event) => {
  const message = JSON.parse(event.data);
  
  if (message.type === 'messages_dropped') {
    console.warn(`Dropped ${message.count} messages`);
    // Request resync to catch up
    resync(50);
  }
};
```

## Heartbeat & Connection Management

### Heartbeat

The server sends periodic heartbeat pings to detect stale connections:

- **Interval**: 30 seconds
- **Timeout**: 10 seconds for pong response
- **Action**: Connection closed if pong not received

### Reconnection Strategy

Implement exponential backoff for reconnection:

```javascript
class RobustWebSocketClient {
  constructor(wsUrl) {
    this.wsUrl = wsUrl;
    this.reconnectDelay = 1000;
    this.maxReconnectDelay = 30000;
    this.connect();
  }

  connect() {
    try {
      this.ws = new WebSocket(this.wsUrl);
      this.ws.onopen = () => {
        this.reconnectDelay = 1000; // Reset on success
        this.resync(50);
      };
      this.ws.onclose = () => this.scheduleReconnect();
      this.ws.onerror = () => this.scheduleReconnect();
    } catch (error) {
      this.scheduleReconnect();
    }
  }

  scheduleReconnect() {
    setTimeout(() => {
      this.reconnectDelay = Math.min(
        this.reconnectDelay * 2,
        this.maxReconnectDelay
      );
      this.connect();
    }, this.reconnectDelay);
  }

  resync(limit) {
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify({
        type: 'resync',
        limit: Math.min(limit, 100)
      }));
    }
  }
}
```

## Performance Considerations

### Memory Usage

- Each client connection maintains a broadcast receiver
- Large limit values increase memory usage
- Recommended: Keep limit ≤ 50 for optimal performance

### Network Bandwidth

- Real-time updates are sent as they occur
- Resync requests may return large payloads
- Consider compression for high-volume scenarios

### Database Load

- Resync queries use indexed lookups
- Partition pruning optimizes date range queries
- Concurrent resync requests are rate-limited

## Error Handling

### Connection Errors

| Error | Cause | Action |
|-------|-------|--------|
| 401 Unauthorized | Invalid token | Refresh token and reconnect |
| 403 Forbidden | Insufficient permissions | Check tenant access |
| 429 Too Many Requests | Rate limit exceeded | Implement backoff |
| 500 Internal Error | Server error | Retry with exponential backoff |

### Message Errors

Invalid messages are silently ignored. Ensure messages conform to the protocol specification.

## Monitoring

### Metrics

Track the following metrics:

- `ws_connections_active`: Current active WebSocket connections
- `ws_messages_sent_total`: Total messages sent to clients
- `ws_messages_dropped_total`: Total messages dropped due to backpressure
- `ws_resync_requests_total`: Total resync requests
- `ws_resync_duration_seconds`: Time to process resync requests

### Logging

All WebSocket events are logged:

- Connection establishment and closure
- Authentication failures
- Resync requests and responses
- Message drops and backpressure events
- Errors and exceptions

## Best Practices

1. **Implement Reconnection Logic**: Use exponential backoff for reliability
2. **Handle Backpressure**: Process messages promptly to avoid drops
3. **Use Appropriate Limits**: Balance between data freshness and performance
4. **Monitor Connection Health**: Track heartbeat and message flow
5. **Validate Messages**: Verify message structure before processing
6. **Clean Up Resources**: Close connections when no longer needed
7. **Test Failure Scenarios**: Simulate network issues and server errors

## Troubleshooting

### Connection Drops

If connections frequently drop:

1. Check network stability
2. Verify token validity
3. Monitor server logs for errors
4. Check rate limiting configuration

### Missing Events

If events appear to be missing:

1. Send resync request to catch up
2. Check if messages were dropped (look for `messages_dropped` notification)
3. Verify subscription filters are correct
4. Review server logs for processing errors

### High Latency

If real-time updates are delayed:

1. Check network latency
2. Monitor server CPU and memory usage
3. Reduce message processing time on client
4. Consider batching updates

## Serialised page cache

`PageCache` (`src/ws/pagination.rs`) keeps the serialised form of recently sent pages so that stepping to the same cursor again does not serialise the page twice. The cache is keyed by tenant plus the raw cursor string, so one tenant can never be served another tenant's page.

| Property | Value |
|----------|-------|
| Bound | 256 pages by default; the oldest entry is dropped first |
| TTL | 5 seconds by default; an older entry is treated as a miss |
| Invalidation | `invalidate_tenant(tenant)` drops all of a tenant's pages; call it when that tenant's events change |
| Wire format | unchanged: the cached string is exactly what the serialiser produced |

Use `PageCache::get_or_serialize(tenant, cursor, || serialize_page())` where a page is about to be sent. The serialiser closure only runs on a miss.
