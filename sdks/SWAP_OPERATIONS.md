# Swap Operations in the Client SDKs

Status tracker for issue #1426 ("Add Swap Operations to the Client SDKs").

## Current state

- **No swap server surface exists yet.** There are no swap REST routes in
  `src/lib.rs` and no swap handlers under `src/handlers/`, and the GraphQL
  schema in `src/graphql` has no swap types.
- `src/swap/mod.rs` is the Phase 2 hand-off boundary only. It defines
  `SwapHandoff`, `SwapOutcome`, and the `SwapEngine` trait, and the default
  `NoopSwapEngine` is inert. Nothing constructs a `SwapHandoff` yet.
- `docs/rfcs/0001-swap-engine-cross-chain-bridge-interface.md` is a draft. It
  does not define HTTP or GraphQL request/response shapes.
- Per-language SDK coverage: `sdks/rust` has tracked source. `sdks/python` and
  `sdks/typescript` have only tests tracked in git, so there is no source to
  extend yet.

Because the wire contract does not exist, typed models and methods for
create/get/list/cancel/exposure cannot be generated from a schema. Hand-writing
them now would encode guessed paths and fields that the server may not ship.

## Prerequisites before SDK work starts

1. The swap engine (or its API layer) publishes an accepted contract: REST
   paths, JSON schemas for create/get/list/cancel/exposure, and the error
   envelope. Record the decision in an ADR per `docs/rfcs/README.md`.
2. The server exposes those routes, with handlers and tests, in `src/handlers/`
   and wires them in `src/lib.rs`.
3. A single source of truth for the schema is agreed, so the SDK models are
   generated or checked against it rather than hand-copied.

## Planned SDK surface (once the contract exists)

- **Models:** typed request/response structs, additive-friendly (unknown fields
  ignored on deserialize, new optional fields do not break existing callers).
- **Methods:** `create`, `get`, `list`, `cancel`, and exposure queries, as a
  `swaps` resource alongside `settlements` and `transactions` in
  `sdks/rust/src/resources/`.
- **Errors:** map the server error envelope to the existing `SynapseError`
  variants in `sdks/rust/src/error.rs`, reusing `NotFound` and `InvalidCursor`
  where they apply. Never include credentials in error messages or logs.
- **Pagination:** list helpers built on `sdks/rust/src/pagination.rs`, using the
  same opaque `next_cursor` convention as settlements.
- **Retry and timeouts:** reuse the defaults in `sdks/rust/src/retry.rs` and
  `sdks/rust/src/client.rs`; do not invent swap-specific defaults.
- **Tests:** serialization and error-mapping unit tests per language, plus one
  integration test per language against a local service.
- **Out of scope:** streaming/subscription clients and offline caching.
