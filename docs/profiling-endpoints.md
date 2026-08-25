# Profiling Endpoints: Status and Intended Access Model

## Current status: fixed, but still unmounted

`src/handlers/profiling.rs` implements four handlers —
`start_profiling`, `get_profiling_status`, `stop_profiling`, `get_flamegraph`
— backed by `ProfilingManager`, which *is* constructed into `AppState` (both
in production, `src/main.rs`, and in `AppState::test_new`). None of the four
handlers are mounted on any router in `src/lib.rs`/`src/main.rs`. This is
unchanged by this fix: `get_flamegraph`'s authorization and path-handling
bugs (see below) are fixed regardless of mount status, so the code is safe
whenever mounting is decided, but the mounting decision itself is out of
scope here — starting a CPU/memory profiling session is a live-request-path
performance action (`start_cpu_profiling`/`start_memory_profiling` spawn a
background task that samples the running process for up to
`duration_secs`), and deciding how to gate/rate-limit that safely is a
separate, larger scoping question than the security fix below.

## What was fixed

`get_flamegraph` previously had no authorization check of any kind, and
built its filesystem path via `format!("{session_id}.svg")` joined onto
`./profiling_data` with the client-supplied `session_id` used as-is — a
classic path-traversal shape (bounded only by the hardcoded `.svg` suffix
that `read_to_string` would need to resolve to an existing file).

Two independent fixes now run before any filesystem access:

1. **`validate_session_id`** — rejects any `session_id` that doesn't match
   the exact shape `start_cpu_profiling`/`start_memory_profiling` mint:
   `profile-(cpu|memory)-<millis>`. A real session ID never contains `/`,
   `..`, or any other path-relevant character, so this closes the traversal
   vector before a path is ever constructed, independent of the
   authorization decision below.
2. **`ensure_flamegraph_path_available`** — fails closed. `ProfilingManager`
   tracks exactly one session at a time (`current_session` is a single slot,
   not a history map), so the only flamegraph that can ever legitimately
   exist to serve is the current session's own, and only after it has
   finished. Every other input — nothing running, a `session_id` that
   doesn't match the current session, or a match that hasn't produced a
   flamegraph yet — is denied. The version this replaces did the reverse: it
   returned an error only for the one narrow "same session, still running"
   case and allowed everything else through, including session IDs matching
   no known session at all.

## Intended access model, once mounted

- All four endpoints are operator/debugging tooling, not customer-facing —
  they belong behind `admin_auth`, mounted on `admin_only_routes` in
  `src/lib.rs` alongside the other `/admin/*` routes, the same way
  `/admin/locks` and (as of this change) `/admin/audit/search` are.
- `start_profiling` triggers real CPU/memory sampling on the live process for
  `duration_secs` (client-supplied, currently unbounded) — before mounting,
  this needs a hard server-side cap on `duration_secs` and probably a
  single-flight guard beyond the existing `is_profiling` check (e.g. a
  minimum cooldown between sessions) so it can't be used to keep the process
  in a permanently-profiled, degraded-throughput state.
- `get_flamegraph`'s fixed fail-closed model above already assumes
  single-session semantics; if profiling history (multiple past sessions)
  is ever wanted, `ProfilingManager` needs a real session store, not a
  single slot, before that model can be extended safely — don't widen
  `ensure_flamegraph_path_available` without doing that first.
