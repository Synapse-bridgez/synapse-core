# Versioning Policy — synapse-cli

## Current stability: 0.x pre-release

`synapse-cli` is currently versioned in the **0.x** range. The command interface
and output formats are still being shaped and may change as the Synapse platform
and underlying SDK evolve.

## Semver rules while `< 1.0`

| Version component | Meaning |
|-------------------|---------|
| **PATCH** (`0.x.Y`) | Backwards-compatible bug fixes, typo corrections, and help-text improvements. |
| **MINOR** (`0.X.0`) | New subcommands/flags **and** breaking changes to existing commands or output formats. Until 1.0 is declared, a minor bump is the signal that scripts using the CLI may need updating. |
| **MAJOR** (`X.0.0`) | Reserved for declaring CLI stability (i.e., the 1.0 release). |

## What counts as a breaking change

- Removing or renaming a subcommand or flag that was previously documented/shipped.
- Changing the meaning of positional arguments for any existing subcommand.
- Altering the machine-readable output format (JSON keys, CSV column order/names) in a backwards-incompatible way.
- Changing the exit-code contract (e.g., re-mapping what exit codes mean).
- Raising the minimum supported Rust edition or MSRV in a way that prevents compilation on the previous toolchain.

## What does NOT count as a breaking change

- Adding new subcommands, flags, or output fields (additive changes).
- Reformatting human-readable table output (column widths, headers) — scripts should use `--output json`.
- Internal refactoring with no observable change to CLI behaviour.
- Dependency version bumps that do not affect CLI behaviour.

## Relationship to synapse-sdk versioning

`synapse-cli` depends on `synapse-sdk`. A breaking SDK release will trigger at
minimum a minor-version bump in `synapse-cli`. The two crates are versioned
independently; see [sdks/rust/VERSIONING.md](../../sdks/rust/VERSIONING.md).

## Path to 1.0

A 1.0 release will be cut once:

1. All core Synapse resources are reachable via stable subcommands.
2. The JSON output schema is considered stable and documented.
3. At least one external team has adopted the CLI in a scripted/automated workflow for 30 days without a breaking-change request.
