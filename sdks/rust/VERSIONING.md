# Versioning Policy — synapse-sdk (Rust)

## Current stability: 0.x pre-release

`synapse-sdk` is currently versioned in the **0.x** range. The API surface is
still being established and may change as the Synapse REST API evolves.

## Semver rules while `< 1.0`

| Version component | Meaning |
|-------------------|---------|
| **PATCH** (`0.x.Y`) | Backwards-compatible bug fixes and documentation updates. |
| **MINOR** (`0.X.0`) | New features **and** breaking changes. Until 1.0 is declared, a minor bump is the signal that callers may need to update their code. |
| **MAJOR** (`X.0.0`) | Reserved for declaring API stability (i.e., the 1.0 release). |

## What counts as a breaking change

- Removing or renaming any public type, field, function, method, or trait impl.
- Changing the signature of a public function or method (parameters, return type, generics).
- Removing a variant from a public `enum` that consumers are expected to match exhaustively.
- Changing the serialised JSON shape of any type that is sent over the wire.
- Raising the minimum supported Rust edition or MSRV in a way that prevents compilation on the previous toolchain.
- Removing a feature flag that was previously stable.

## What does NOT count as a breaking change

- Adding new public items (types, methods, fields with `#[non_exhaustive]`).
- Adding new optional feature flags.
- Internal refactoring with no change to the public API.
- Dependency version bumps that do not affect the public API.

## Path to 1.0

A 1.0 release will be cut once:

1. All core Synapse resources (workspaces, events, subscriptions, reconciliation reports) are covered by a stable client API.
2. The REST contract is considered stable by the Synapse platform team.
3. At least one production integration has been running against the SDK for 30 days without a breaking-change requirement.
