# Webhook Event Catalog

This document describes the available webhook event types emitted by the transaction state machine.

## Granular Event Types (Recommended)

Starting with version X.Y.Z, the following granular event types are available, allowing consumers to subscribe only to the transitions they care about:

### `transaction.created`
Emitted when a new transaction is initially created.

**Payload schema:**
- `event_type`: "transaction.created"
- `transaction_id`: UUID
- `timestamp`: ISO 8601 datetime
- `data`: Transaction object

### `transaction.matched`
Emitted when a transaction transitions to the "processing" state.
This indicates the transaction has been matched/validated and is ready for processing.

**Transitions:**
- pending → processing
- failed → pending (requeue)
- dlq → pending (recovery)
- pending_review → pending (re-open)

**Payload schema:**
- `event_type`: "transaction.matched"
- `transaction_id`: UUID
- `timestamp`: ISO 8601 datetime
- `data`: Transaction object

### `transaction.completed`
Emitted when a transaction successfully completes.

**Transitions:**
- pending → completed
- processing → completed
- pending_review → completed

**Payload schema:**
- `event_type`: "transaction.completed"
- `transaction_id`: UUID
- `timestamp`: ISO 8601 datetime
- `data`: Transaction object with final status

### `transaction.failed`
Emitted when a transaction enters a failed state.

**Transitions:**
- pending → failed
- processing → failed
- pending_review → failed

**Payload schema:**
- `event_type`: "transaction.failed"
- `transaction_id`: UUID
- `timestamp`: ISO 8601 datetime
- `data`: Transaction object with error details

## Legacy Generic Event (Deprecated)

### `transaction.update`
The legacy event type that was previously emitted for all transaction status changes.

**Deprecation note:** This event type continues to be emitted by default for backward compatibility, but new consumers should use the granular event types above.

To opt into granular events only (disable the legacy event), configure your webhook endpoint's filter rules:

```json
{
  "event_types": [
    "transaction.created",
    "transaction.matched",
    "transaction.completed",
    "transaction.failed"
  ]
}
```

## Exactly-Once Delivery Guarantee

All webhook events, whether granular or legacy, are subject to the exactly-once delivery guarantee enforced by the `webhook_dispatcher`. Each event is assigned a unique delivery ID and tracked through retry attempts up to `MAX_ATTEMPTS`.

## Filter Rules

The webhook filter rules engine (`migrations/20260425000000_add_webhook_filter_rules.sql`) controls which event types are delivered to each endpoint. Endpoints without explicit filter configuration receive both granular and legacy events for backward compatibility.

### Opt-In to Granular Events

To enable granular events and disable the legacy generic event:

1. Update the webhook endpoint configuration with:
```json
{
  "filter_rules": {
    "event_types": [
      "transaction.created",
      "transaction.matched",
      "transaction.completed",
      "transaction.failed"
    ]
  }
}
```

2. The dispatcher will respect these filters and emit only the listed event types.

### Transition Reference Table

The following table documents all valid state machine transitions and their corresponding event types:

| From | To | Event Type |
|---|---|---|
| pending | processing | transaction.matched |
| pending | completed | transaction.completed |
| pending | failed | transaction.failed |
| processing | completed | transaction.completed |
| processing | failed | transaction.failed |
| failed | pending | transaction.matched |
| dlq | pending | transaction.matched |
| pending_review | completed | transaction.completed |
| pending_review | failed | transaction.failed |
| pending_review | pending | transaction.matched |
| any | any (same) | (no event emitted) |

## Implementation Notes

- Granular events are implemented via `transition_to_event_type()` mapping function
- The event type is determined at the time of status change
- No event is emitted for idempotent same-state transitions
- Filter rules are evaluated per-endpoint during delivery
- Delivery tracking (attempt count, retry times) applies equally to all event types
