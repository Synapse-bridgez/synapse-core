# Synapse Go SDK

Go client SDK for the Synapse API — full parity with the Rust SDK (`sdks/rust`).

## Installation

```bash
go get github.com/synapse-bridgez/synapse-core/sdks/go
```

## Quick Start

```go
import synapse "github.com/synapse-bridgez/synapse-core/sdks/go/synapse"

// Public API client (X-API-Key authentication)
client := synapse.NewClient("https://api.example.com", "your-api-key")

// Fetch a transaction
tx, err := client.Transactions().Get(ctx, "550e8400-e29b-41d4-a716-446655440000")
if err != nil {
    log.Fatal(err)
}
fmt.Printf("status: %s\n", tx.Status)

// List transactions (paginated)
page, err := client.Transactions().List(ctx, synapse.ListParams{
    Limit: synapse.PtrInt64(50),
})

// Admin operations use Authorization: Bearer <key>
admin := synapse.NewAdminClient("https://api.example.com", "admin-key")
locks, err := admin.Locks().List(ctx)
```

## Resources

| Resource | Methods |
|---|---|
| `Transactions()` | `Get`, `List`, `Search`, `Export` |
| `Settlements()` | `Get`, `List` |
| `Stats()` | `Status`, `Daily`, `Assets`, `CacheMetrics` |
| `Health()` | `Check` |
| `GraphQL()` | `Query` |
| `Events()` | `Watch`, `Reconnect`, `ReconnectStatus` |
| Admin: `Locks()` | `List`, `ForceRelease` |
| Admin: `Reconciliation()` | `ListReports`, `Run` |
| Admin: `Settlements()` | `UpdateStatus` |
| Admin: `BulkStatus()` | `Update` |
| Admin: `Dlq()` | `List`, `Requeue` |
| Admin: `WebhookReplay()` | `ListFailed`, `Replay`, `ReplayBatch` |

## Error Handling

All SDK errors are `*SynapseError`:

```go
var se *synapse.SynapseError
if errors.As(err, &se) && se.StatusCode == 404 {
    fmt.Println("not found")
}

// Convenience helpers
if synapse.IsNotFound(err) { ... }
if synapse.IsUnauthorized(err) { ... }
```

## Closes

Issue #1341 — Build a Go SDK with parity to the Rust SDK.
