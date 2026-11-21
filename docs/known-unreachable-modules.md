# Known Unreachable Modules

`scripts/check-unreachable-modules.sh` fails CI on any `src/*.rs` file that
isn't reachable from `src/lib.rs` or `src/main.rs` via a `mod`/`pub mod`
declaration chain — i.e. a file that exists on disk but never compiles into
the crate. That check was added by the same change that produced this file
(see `docs/adr/007-remove-orphaned-hexagonal-and-payments-modules.md`), and
running it once against `main` turned up more orphans than the specific
issue it was written for. Those extra orphans are listed here, out of scope
for that change, so the new check is green without silently sweeping them
under the rug or expanding that PR's diff to fix unrelated dead code.

Each entry should eventually be either fixed (declared + wired to a live
caller, with tests) or deleted, at which point it should be removed from
this file — an entry sitting here is a flagged gap, not a sanctioned
permanent exception.

| File | What it is | Why it's orphaned |
|------|------------|--------------------|
| `src/services/circuit_breaker.rs` | A full circuit-breaker implementation (open/half-open/closed state machine, Redis-backed) for external API calls — the pattern ADR-002 documents. | `src/stellar/client.rs` already has its own live circuit breaker built directly on the `failsafe` crate (see `stellar/client.rs:126,139,164`). This file duplicates that responsibility with a different implementation and is never constructed anywhere. |
| `src/handlers/webhook_refactored.rs` | A "refactored" webhook handler module with its own validation/error-handling structure, per its own doc comment. | Never declared in `src/handlers/mod.rs`. Looks like an in-progress rewrite of `src/handlers/webhook.rs` that was never finished or swapped in. |
| `src/handlers/feature_flag_examples.rs` | Example handlers demonstrating feature-flag usage. | Never declared in `src/handlers/mod.rs`. Reads as intentionally-illustrative sample code, not a subsystem meant to run — but it should either be moved under `examples/` (where dead code is expected) or deleted, not left as an uncompiled file under `src/handlers/`. |
| `src/db/query_builder.rs` | A dynamic SQL query builder for admin/reporting filters, with its own security-notice doc comment about string interpolation. | Never declared in `src/db/mod.rs`. Notably, `.github/workflows/rust.yml`'s own comments describe this file as covering "dynamic WHERE clause construction" for pagination unit-test coverage — that comment is stale; this file's tests do not currently run in CI at all because the file isn't part of the crate. |
| `src/services/lock_examples.rs` | Example call sites (`process_transaction_with_lock`, `long_running_with_lock`, `process_with_helper`) showing how to use `LockManager`. | Never declared in `src/services/mod.rs`. Related to, but not fixed by, this issue's Part E (which wires `LeaderElection` into `lock_registry` — a different mechanism than `LockManager`/`FairLockManager`, which `lock_examples.rs` demonstrates and which also has no live constructor outside its own `#[ignore]`d tests). |

## Keeping this list in sync

`scripts/check-unreachable-modules.sh` treats this file as its allowlist: it
reads `ALLOWLIST_FILE="docs/known-unreachable-modules.md"` and skips any
`src/*.rs` file whose path appears verbatim (as `src/<path>`) in this
document. So the table above must list **exactly** the files the script would
otherwise flag — no more, no less.

To regenerate the list after fixing or deleting an orphan:

```bash
# 1. Run the check. Any file it reports is either a new orphan (fix or delete
#    it) or an entry that is missing from this doc.
./scripts/check-unreachable-modules.sh

# 2. List the paths this doc currently allowlists, to compare against the
#    script's output:
grep -oE 'src/[A-Za-z0-9_/]+\.rs' docs/known-unreachable-modules.md | sort -u
```

When the script passes, the two sets match. Remove a row here as soon as its
file is declared/wired or deleted; add a row only for a pre-existing orphan
that is genuinely out of scope of the change at hand.

## References

- `scripts/check-unreachable-modules.sh`
- `docs/adr/007-remove-orphaned-hexagonal-and-payments-modules.md`
