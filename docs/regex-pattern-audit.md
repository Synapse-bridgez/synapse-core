# Regex Pattern Audit (ReDoS)

Audit of every regular-expression pattern source in `src/`, classified as
**static** (developer-authored, safe by review) or **dynamic**
(operator/tenant-supplied, needs runtime protection). Issue #1313.

The inventory is enforced by `src/validation/regex_audit.rs`, which runs in
`cargo test --lib`. Update this document and that module together.

## Engines

| Engine | Used by | Backtracking? |
|---|---|---|
| `regex` 1.x | direct `Regex::new` calls | No. Finite-automata engine with linear-time matching in input length, so it cannot backtrack catastrophically. |
| `fancy-regex` 0.11 (via `jsonschema` 0.17) | JSON-schema `pattern` / `patternProperties` | Only for patterns that use lookaround or backreferences. Other patterns are wrapped and delegated to the `regex` crate (`RegexImpl::Wrap`), so they match in linear time. |

## Inventory

### Direct `regex` crate patterns

| Location | Pattern | Input | Class |
|---|---|---|---|
| `src/auth/input_validation.rs` `api_key_pattern` | `^[a-zA-Z0-9_\-\.]+$` | API key header (untrusted) | Static |
| `src/graphql/validation.rs` `alphanumeric_pattern` | `^[a-zA-Z0-9_\-\.]+$` | GraphQL arguments (untrusted) | Static |
| `src/graphql/validation.rs` `stellar_account_pattern` | `^[G][A-Z0-9]{55}$` | GraphQL arguments (untrusted) | Static |
| `src/telemetry/input_validation.rs` `IDENTIFIER_PATTERN` | `^[a-zA-Z0-9_\-\.]+$` | span names | Static (named const, allowlisted in the guard) |
| `src/handlers/profiling.rs` `SESSION_ID_RE` | `^profile-(cpu\|memory\|continuous)-[0-9]{1,20}$` | profiling `session_id` path parameter (untrusted) | Static |
| `src/error.rs` (test module) | `ERR_[A-Z0-9_]*[0-9]{3}` | `docs/error-catalog.md` in a unit test | Static, test-only |

All of them run on the linear-time engine.

### JSON-schema patterns (`src/validation/schemas.rs`)

| Schema | Field | Pattern | Class |
|---|---|---|---|
| `callback_schema_v1` | `stellar_account` | `^G[A-Z2-7]{55}$` | Static |
| `callback_schema_v1` | `amount` | `^[0-9]+(\.[0-9]+)?$` | Static |
| `callback_schema_v1` | `asset_code` | `^[A-Z]{3,12}$` | Static |
| `webhook_schema_v1` | none | none | none |

None of these patterns use lookaround or backreferences, so `fancy-regex`
delegates them to the linear-time engine. None has a nested unbounded
quantifier. `amount`'s `(\.[0-9]+)?` group has a bounded outer `?`.

### Not regex

- `src/cache/validation.rs` `validate_pattern`: Redis `KEYS`-style glob
  (`prefix*`), validated by hand-written character checks. No regex engine.
- `mockito::Matcher::Regex` in `src/services/reconciliation.rs` and
  `src/stellar/client.rs`: test-only HTTP mocks.

### Dynamic sources

**None.** No operator- or tenant-supplied pattern reaches a regex engine at
the time of this audit. A future feature that accepts patterns (for example,
webhook filter rules or memo routing) must add creation-time complexity
bounds and a match-time limit before it lands. The guard below fails CI when
that happens.

## Guard (`src/validation/regex_audit.rs`)

- `every_regex_constructor_takes_a_static_pattern`: scans `src/` for
  `Regex::new(`, `RegexBuilder::new(` and `RegexSet::new(` and fails if an
  argument is neither a string literal nor a const listed in
  `REVIEWED_PATTERN_CONSTS`.
- `reviewed_pattern_consts_are_still_used`: prevents a stale allowlist.
- `schema_patterns_run_on_the_linear_time_engine`: every schema pattern must
  compile with the `regex` crate, which rejects lookaround and backreferences.
- `schema_patterns_have_no_nested_unbounded_quantifiers`: rejects shapes such
  as `(a+)+`, `(\d*)*` and `(x+){2,}`.

**Known limits of the guard:** it is a line-based source scan, so a
constructor split across lines with the pattern on the next line is recorded
with an empty argument and fails the check (a false positive, never a silent
pass). Overlapping alternation such as `(a|aa)+` is not detected by the
nested-quantifier check, but it only matters on the backtracking engine, and
the linear-engine check already keeps schema patterns off that engine.
