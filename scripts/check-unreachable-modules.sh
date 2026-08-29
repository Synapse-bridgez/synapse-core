#!/usr/bin/env bash
# Regression guard for the pattern this repo keeps hitting: a well-built,
# well-tested file sits on disk under src/ but was never `mod`-declared by
# its parent, so it silently never compiles into the crate at all. Every one
# of the following was exactly this bug: src/domain, src/ports, src/adapters,
# src/use_cases (never declared in src/lib.rs), src/handlers/auth.rs (never
# declared in src/handlers/mod.rs), src/handlers/admin/audit.rs and
# compliance.rs (never declared in src/handlers/admin/mod.rs). In every case
# the file itself compiled cleanly in isolation and had no compiler warning
# to surface the gap — `cargo build` only ever sees files reachable from a
# `mod` declaration chain rooted at src/lib.rs or src/main.rs, so an
# undeclared file is invisible to it by construction.
#
# Two checks:
#
# 1. STRUCTURAL (hard failure): every .rs file under src/ must be reachable
#    from src/lib.rs *or* src/main.rs (some modules, e.g. src/cli.rs, are
#    binary-only and declared under main.rs, not lib.rs) via an unbroken
#    `mod`/`pub mod` declaration chain. This is mechanical and exactly
#    matches the root cause of every regression listed above.
#
#    Files listed in docs/known-unreachable-modules.md are pre-existing
#    orphans discovered by this check that are out of scope for the change
#    that introduced it — see that file for what they are and why they were
#    left alone rather than fixed or deleted here. Removing an entry from
#    that allowlist (because it's been fixed) is encouraged; adding a new
#    entry to duck this check is not the intended use — declare or delete
#    the file instead.
#
# 2. HEURISTIC (warning only): every top-level `pub mod X;` declared in
#    src/lib.rs should have at least one reference to `X::` somewhere outside
#    its own src/X/ directory. This catches a module that compiles but has
#    drifted to zero live callers (e.g. src/payments/ before it was removed
#    — see docs/adr/007-remove-orphaned-hexagonal-and-payments-modules.md).
#    It is warning-only because a plain `grep` for `X::` cannot see
#    re-exported symbols used without the module-qualified path, so it is
#    prone to false positives; treat a warning here as "go look," not
#    "automatically wrong."
#
# Usage: ./scripts/check-unreachable-modules.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

ALLOWLIST_FILE="${UNREACHABLE_MODULES_ALLOWLIST_FILE:-docs/known-unreachable-modules.md}"

is_reachable_from() {
  local root_file="$1"
  shift
  local parts=("$@")
  local parent_dir="src"
  local parent_file="$root_file"
  local part

  for part in "${parts[@]}"; do
    if [[ ! -f "$parent_file" ]] || ! grep -qE "^[[:space:]]*(pub[[:space:]]+)?mod[[:space:]]+${part}[[:space:]]*;" "$parent_file"; then
      return 1
    fi
    parent_dir="$parent_dir/$part"
    if [[ -f "$parent_dir/mod.rs" ]]; then
      parent_file="$parent_dir/mod.rs"
    else
      parent_file="$parent_dir.rs"
    fi
  done
  return 0
}

# ---------------------------------------------------------------------------
# Check 1: structural reachability
# ---------------------------------------------------------------------------

ORPHANED=()

while IFS= read -r -d '' file; do
  rel="${file#src/}"
  [[ "$rel" == "lib.rs" || "$rel" == "main.rs" ]] && continue

  if [[ -f "$ALLOWLIST_FILE" ]] && grep -qF "src/$rel" "$ALLOWLIST_FILE"; then
    continue
  fi

  if [[ "$(basename "$rel")" == "mod.rs" ]]; then
    modpath="${rel%/mod.rs}"
  else
    modpath="${rel%.rs}"
  fi

  IFS='/' read -ra parts <<< "$modpath"

  if is_reachable_from "src/lib.rs" "${parts[@]}" || is_reachable_from "src/main.rs" "${parts[@]}"; then
    continue
  fi

  ORPHANED+=("src/$rel (crate::${modpath//\//::} is not declared by any mod/pub mod chain from src/lib.rs or src/main.rs)")
done < <(find src -name "*.rs" -print0)

if [[ "${#ORPHANED[@]}" -gt 0 ]]; then
  echo "::error::Found src/*.rs files unreachable from src/lib.rs and src/main.rs (never mod-declared, so they never compile into the crate):"
  for o in "${ORPHANED[@]}"; do
    echo "  - $o"
  done
  echo "::error::Either declare the missing 'mod'/'pub mod' in the parent module, delete the file if it's abandoned, or (only for a pre-existing orphan genuinely out of scope of your change) add it to ${ALLOWLIST_FILE} with a reason. See docs/adr/007-remove-orphaned-hexagonal-and-payments-modules.md for the precedent."
  exit 1
fi

echo "check-unreachable-modules: structural check passed — every src/*.rs file is reachable from src/lib.rs or src/main.rs."

# ---------------------------------------------------------------------------
# Check 2: zero-external-caller heuristic for top-level pub mods
# ---------------------------------------------------------------------------

TOP_LEVEL_MODS=$(grep -E "^pub mod [a-z_]+;" src/lib.rs | sed -E 's/^pub mod ([a-z_]+);/\1/')

for m in $TOP_LEVEL_MODS; do
  # Search everywhere except the module's own directory/file. This is a
  # heuristic, not a proof: it can't see symbols reached only via a
  # re-exported type name (no `${m}::` qualifier), so treat a warning as
  # "go look," not "definitely dead."
  external_hits=$(grep -rln "\b${m}::" src 2>/dev/null | grep -v "^src/${m}/" | grep -v "^src/${m}\.rs" || true)

  if [[ -z "$external_hits" ]]; then
    echo "::warning::pub mod '${m}' (declared in src/lib.rs) has no '${m}::' references anywhere outside src/${m}/ — verify it still has a live caller, or that it's reached only via re-exported types (this heuristic can't see those). See docs/adr/007-remove-orphaned-hexagonal-and-payments-modules.md for how this class of drift gets caught and resolved."
  fi
done

echo "check-unreachable-modules: heuristic external-caller check complete (warnings, if any, are above)."
