# Supply-Chain Policy (cargo-deny)

## Overview

`deny.toml` at the repo root defines the license, security-advisory, and
dependency-source policy for this crate, enforced by [`cargo-deny`]. It is
checked by the `supply-chain-audit` job in `.github/workflows/rust.yml`:

- On every push/PR, same as the other CI jobs.
- Once daily on a `schedule` trigger, because a new RustSec advisory can be
  published against a dependency version that is already merged, with no
  code change to trigger a normal push/PR run.

A scheduled-run failure has no commit or PR to show a red check on, so the
job also opens (or comments on, if one is already open) a GitHub issue
labeled `supply-chain` — see the `File or update tracking issue for
scheduled-run failure` step. That is the "someone gets paged" mechanism for
this repo today; if a Slack/PagerDuty integration is added to CI later,
replace that step's target rather than adding a second parallel channel.

Interaction with `.github/dependabot.yml`'s patch-update auto-merge: GitHub's
native auto-merge only completes a PR once its *required* status checks
pass. `supply-chain-audit` is now one of the checks that runs on every PR,
so a Dependabot patch bump that introduces a new advisory or an
incompatible license is blocked from auto-merging the same way any other
failing check blocks it — provided `supply-chain-audit` is added to this
repo's required-status-checks branch protection list (a GitHub repo
setting, not something set in this workflow file — verify it's actually
required, not just present).

[`cargo-deny`]: https://embarkstudios.github.io/cargo-deny/

## What each section of `deny.toml` checks

- **`[advisories]`** — RustSec security/unmaintained/unsound/yanked
  advisories against everything in `Cargo.lock`.
- **`[licenses]`** — every dependency's SPDX license must appear in the
  `allow` list (including transitive dependencies).
- **`[bans]`** — currently `warn`-only for duplicate versions and wildcard
  version requirements; does not fail the build.
- **`[sources]`** — only crates.io is an allowed registry source.

## Running it locally

```sh
brew install cargo-deny   # or: cargo install cargo-deny
cargo deny check
```

Run this before pushing a dependency change (`Cargo.toml`/`Cargo.lock`
edits) — it's fast (no compilation) and catches what CI will catch.

## Interpreting a failure

`cargo deny check` prints one block per finding, each with a `RUSTSEC-*` ID
(for advisories) or the offending SPDX license string, and a dependency path
showing what pulled it in. Three outcomes:

1. **A safe upgrade exists** (`cargo update -p <crate>`, or a `Cargo.toml`
   version bump for a direct dependency). Prefer this — it's the actual fix.
   Confirm with `cargo check` afterward that nothing downstream broke.
2. **No safe upgrade exists, or the safe upgrade is a major-version bump
   that's out of scope for the change you're making.** This is a legitimate
   exception, not something to silently suppress. Add the advisory ID to
   `[advisories].ignore` in `deny.toml` with a comment explaining: what pulls
   it in, why there's no low-risk fix today, and the date you reviewed it.
   See the existing entries in `deny.toml` for the format.
3. **A new, legitimately-licensed transitive dependency.** Add its SPDX
   identifier to `[licenses].allow` with a comment naming the crate(s) that
   require it. Don't add a license you haven't looked up — check it's
   OSI-approved/FSF-free and compatible with this project's MIT license
   before allowing it.

## Reviewing existing exceptions

Every entry in `[advisories].ignore` has a "Reviewed <date>" comment. When
touching a dependency near one of these (or at least twice a year), re-run
`cargo deny check` with the entry temporarily removed and see if a safe
upgrade has since become available. If so, apply it and delete the ignore
entry instead of just bumping the review date.

## Known accepted exceptions (as of 2026-08-24)

| Advisory | Crate | Why deferred |
|---|---|---|
| RUSTSEC-2024-0363 | sqlx 0.7.4 | Fix requires sqlx 0.8 (semver-major); `sqlx::query` is called at ~117 sites across ~48 files. Needs a dedicated upgrade PR. |
| RUSTSEC-2024-0370 | proc-macro-error | Unmaintained, no replacement upstream. |
| RUSTSEC-2024-0436 | paste | Unmaintained, no safe upgrade; compile-time-only macro crate. |
| RUSTSEC-2024-0437 | protobuf (via `prometheus`) | Fix requires a `prometheus` major-version bump. |
| RUSTSEC-2025-0111 | tokio-tar (dev-only, via testcontainers) | No safe upgrade; not present in the production binary. |
| RUSTSEC-2025-0134 | rustls-pemfile | Unmaintained, no safe upgrade. |
| RUSTSEC-2026-0194 / -0195 | quick-xml (via `pprof`'s flamegraph feature) | No safe upgrade without a newer `inferno` release; only parses profiler-generated SVGs, not untrusted input. |
| RUSTSEC-2026-0204 / -0258 | h2 0.3.27 (via axum 0.6) | Fix requires an axum 0.6 → 0.7+/0.8 major upgrade. The h2 0.4.x instance in the same tree (used by tonic/reqwest) is already patched to 0.4.16. |

None of these are ignored blindly — each is a real, reviewed trade-off
between the fix's blast radius and the risk it defers. The sqlx and axum
upgrades in particular should be tracked as their own follow-up issues
rather than attempted as part of routine dependency maintenance.

As of August 28, 2026, this check should remain a required branch-protection
gate rather than an informational-only workflow, otherwise dependency policy
failures can still merge through an unrelated green path.
