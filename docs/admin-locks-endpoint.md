# GET /admin/locks: What It Observes

## Current status

`GET /admin/locks` reads `lock_manager::lock_registry().snapshot()` and
returns it as-is — the handler (`src/handlers/admin/locks.rs`) has no other
logic. As of this fix, the registry is written to by:

- **`LeaderElection`** (`src/services/lock_manager.rs`) — registers under
  `resource: "processor:leader"` on every successful acquire/renew, and
  deregisters on losing leadership or when a leader-gated job (currently
  `ReconciliationJob`, the only live caller) finishes its cycle. This is the
  coordination mechanism actually used in production today.

The registry is **not** written to by:

- **`LockManager`/`FairLockManager`** (same file) — both correctly call
  `lock_registry().register()`/`.deregister()` on acquire/release, but
  neither has a live constructor anywhere outside their own `#[ignore]`d
  tests and `src/services/lock_examples.rs` (itself not `mod`-declared, so
  not even compiled in — see `docs/known-unreachable-modules.md`). If either
  is ever wired into a live code path, its lock activity will show up here
  automatically, with no further changes needed — the registry write path
  already exists and is tested.

## What this means for on-call

- An entry with `resource: "processor:leader"` reflects real leader-election
  state: which instance currently holds the lease, refreshed on every
  successful renewal.
- `overdue: true` on that entry means the last successful renewal is more
  than `2 × LEADER_LEASE_SECS` (60s) old — either a real problem (the leader
  crashed or lost Redis connectivity mid-lease) or, less urgently, that a
  leader-gated job that acquired leadership hasn't released its registration
  yet (the reconciliation job releases explicitly on completion; a future
  leader-gated job that doesn't call `release_leadership_registration()`
  would show as overdue between the lease's natural Redis expiry and the
  next scheduled run — check the job's own logs/schedule before treating
  this as an incident).
- An **empty response** now means "no active `LeaderElection` lease and no
  active `LockManager`/`FairLockManager` lock" — which, given the latter two
  have no live callers today, in practice means "no leader election lease
  currently held." It does not yet mean "no lock-like coordination of any
  kind is happening" in a fully general sense — only `LeaderElection`,
  `LockManager`, and `FairLockManager` are covered; if another coordination
  mechanism is added to this codebase in the future, it needs to be wired
  into the same registry (or this doc updated) for this endpoint's "nothing
  is stuck" reading to stay trustworthy.
