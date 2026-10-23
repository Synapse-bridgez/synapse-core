# Swap API error contract

Swap HTTP failures use [RFC 7807](https://www.rfc-editor.org/rfc/rfc7807)
`application/problem+json`. The fields `type`, `title`, `status`, `detail`, and
`code` are always present. `type` is `urn:synapse:swap:problem:<code>`.
An outer request logger may add `request_id`. Responses are not cacheable.

| Code | HTTP | Meaning / recovery |
| --- | --- | --- |
| SWAP_VALIDATION | 400 | Invalid input; correct the request |
| SWAP_UNSUPPORTED_ASSET | 422 | Select a supported asset or pair |
| SWAP_STALE_QUOTE | 409 | Refresh the quote; for streams reconnect without a cursor |
| SWAP_LIMIT_EXCEEDED | 429 | Reduce subscriptions or connections; respect Retry-After |
| SWAP_EXPIRED | 410 | Request a fresh quote |
| SWAP_UPSTREAM_FAILURE | 502 | Liquidity provider failure |
| SWAP_SETTLEMENT_FAILURE | 502 | Settlement failure; reconcile before retrying a mutation |
| SWAP_INTERNAL | 500 | Unexpected internal failure |
| SWAP_UNAUTHORIZED | 401 | Supply a valid active tenant key |
| SWAP_FORBIDDEN | 403 | Access denied |
| SWAP_NOT_FOUND | 404 | Unknown swap resource |
| SWAP_METHOD_NOT_ALLOWED | 405 | Use a supported method |
| SWAP_UNAVAILABLE | 503 | Admission rejected; respect Retry-After |

Codes and their meanings/statuses are additive-only across releases: never
rename, remove, recycle, or change the classification of an existing code.
Clients should tolerate unknown future codes and fall back to the HTTP status.
Messages are explanatory and are not a parsing contract. HTTP 429 and 503 include
`Retry-After: 1` unless the admission controller supplies its configured delay.

`src/error.rs` owns the enum and rendering. Swap handlers return `SwapApiError`;
the swap router's outer problem middleware also normalizes framework/extractor
rejections. Unknown infrastructure errors always become `SWAP_INTERNAL`, with
the cause logged server-side. Do not pass raw SQL, upstream response bodies,
secrets, tenant identifiers, or debug strings to clients. WS frames and SSE
events after successful upgrade use the streaming protocol documented separately;
HTTP problem responses apply before establishment.

This contract is scoped to swap APIs. Existing Phase 1 error responses retain
their current shape.
