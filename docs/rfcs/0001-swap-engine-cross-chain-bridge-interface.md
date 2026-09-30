# RFC 0001: Swap-Engine and Cross-Chain Bridge Interface

- **Status:** Draft
- **Author(s):** @maintainers
- **Created:** 2024-01-01
- **Review period:** 14 days from the date this RFC is opened as a PR
- **Related ADR:** _to be filed on acceptance (see `docs/adr/`)_

> This is a worked-example RFC authored to validate the process described in
> `docs/rfcs/README.md`. It is intentionally lightweight: fill in the sections
> below, keep it short, and open a PR against `docs/rfcs/`.

## Summary

Define a stable interface between the swap engine and cross-chain bridge
adapters so that Phase 2/3 work (new chains, new bridge providers) can proceed
without repeatedly changing the swap engine's core.

## Problem

The swap engine currently talks to bridge providers through ad-hoc, provider-
specific code paths. Adding a new chain or bridge requires touching the swap
engine itself, which couples unrelated concerns and makes each integration a
larger, riskier change than it should be. There is no agreed contract for what
a bridge adapter must provide, so behavior (quoting, fee accounting, failure
handling) varies between providers.

## Proposed Approach

Introduce a narrow `BridgeAdapter` interface that the swap engine depends on,
with one implementation per provider:

- `quote(request) -> Quote` — pricing and estimated fees for a swap.
- `execute(quote) -> ExecutionHandle` — submit the swap and return a handle.
- `status(handle) -> Status` — poll or subscribe for completion/failure.
- `refund(handle) -> RefundResult` — recover funds on failure where supported.

The swap engine is refactored to depend only on this interface. Provider-
specific details (RPC endpoints, chain IDs, token mappings) live inside each
adapter. Adapters are registered at startup rather than referenced directly.

## Alternatives Considered

- **Keep provider-specific code paths in the swap engine.** Rejected: this is
the status quo that motivates the RFC; it does not scale to more providers.
- **A single generic adapter with runtime branching.** Rejected: pushes
provider differences into conditionals inside one large module, which is harder
to test and review than separate adapters.
- **Adopt an existing third-party bridge abstraction.** Deferred: worth
revisiting, but no candidate currently covers the chains and providers in scope.

## Open Questions

- Should `status` be poll-based, event-based, or both? Polling is simpler to
  start with; events may be needed for responsive UX.
- How should partial fills and multi-hop routes be represented in `Quote`?
- What is the minimum set of adapter capabilities the swap engine may assume,
  versus feature-detecting optional ones (e.g. `refund`)?
- Do we version the interface, and if so, how are breaking changes rolled out
  across adapters?

## Outcome

_To be completed on acceptance._ If accepted, this RFC must result in a
corresponding ADR under `docs/adr/` recording the final decision, and this
section should link to it. If the RFC is withdrawn or deferred, record that
state here with a one-line reason (see `docs/rfcs/README.md`).
