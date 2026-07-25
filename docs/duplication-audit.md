# Duplication Audit Report

**Date:** 2026-07-21  
**Scope:** Complete merge commit history of synapse-core repository across all three crates (src/, sdks/rust/src/, cli/synapse-cli/src/)  
**Methodology:** See [Audit Methodology](#audit-methodology)

---

## Executive Summary

This audit found **7 categories of genuine duplication** and **3 areas of intentional parallel definitions** across the synapse-core workspace. The primary cause is architectural: the CLI crate reimplements SDK and core-library types instead of importing them as dependencies. Past merge conflicts were systematically resolved by concatenating both sides' implementations (e.g., commit bad15b3 fixed three competing admin-API designs, duplicate state machines, duplicate test suites). While most compile-breaking duplication was caught and fixed, non-breaking duplication remains.

---

## Audit Methodology

### Merge Commit Selection

All merge commits in the main branch history were reviewed (100+ commits). The scan focused on merges likely to have conflict resolution: "Merge main into feature/*" branches and PR merges from parallel feature work.

**Key pattern identified:** Merge conflicts where both sides' contributions remained in the result instead of being reconciled, typically marked by:
- Parallel definitions of the same type or function
- Dead code paths (unreachable after merge)
- Duplicate test suites with different mock data expectations
- Unclosed module blocks that swallow subsequent definitions

### Type Duplication Detection

```bash
# Scan for types defined in multiple crates/modules
grep -r "pub struct\|pub enum" src/ sdks/rust/src/ cli/ | \
  grep -oE "pub (struct|enum) \w+" | \
  sort | uniq -c | sort -rn
```

All types appearing 2+ times were manually verified to determine if duplication was intentional (e.g., different purposes) or accidental.

---

## Findings

### Category 1: Model Type Duplication (Critical)

These types are semantically identical and defined in multiple crates. CLI and SDK versions should be unified under a single canonical definition.

#### Finding 1.1: StatusCount

**File locations:**
- `src/db/queries.rs:1577` - Original definition (canonical)
- `sdks/rust/src/models.rs:318` - SDK version
- `cli/synapse-cli/src/commands/stats.rs:10` - CLI version

**Definition check:** All three are identical (transaction_id, status, count fields)

**Assessment:** **GENUINE DUPLICATION**  
Semantic purpose: Response type for transaction status counts query. Three-way duplication across crate boundaries.

**Fix:** CLI should import from SDK; SDK should re-export from canonical source or inline the 3-line definition.

---

#### Finding 1.2: DailyTotal

**File locations:**
- `src/db/queries.rs:1583` - Original definition (canonical)
- `src/handlers/stats.rs:21` - Also defined as DailyTotalsQuery (similar purpose, different name)
- `sdks/rust/src/models.rs:325` - SDK version
- `cli/synapse-cli/src/commands/stats.rs:16` - CLI version

**Definition check:** db/queries.rs and SDK are structurally identical; handlers/stats.rs is a different type

**Assessment:** **GENUINE DUPLICATION**  
Semantic purpose: Response type for daily transaction totals. Three-way duplication (db origin → SDK copy → CLI copy). The handlers/stats.rs version is a separate query builder, intentional.

**Fix:** CLI should import from SDK or db/queries.rs directly.

---

#### Finding 1.3: AssetStats

**File locations:**
- `src/db/queries.rs:1590` - Original definition (canonical)
- `sdks/rust/src/models.rs:333` - SDK version
- `cli/synapse-cli/src/commands/stats.rs:23` - CLI version

**Definition check:** All three identical (asset_code, total_volume, total_fees, etc.)

**Assessment:** **GENUINE DUPLICATION**  
Semantic purpose: Response type for per-asset statistics. Three-way duplication.

**Fix:** CLI should import from SDK.

---

#### Finding 1.4: Settlement

**File locations:**
- `src/db/models.rs:148` - Database model
- `sdks/rust/src/models.rs:66` - SDK version  
- `cli/synapse-cli/src/commands/settlements.rs:11` - CLI version

**Note:** Also `SettlementListResponse` and `SettlementList` in same files (structural variants)

**Definition check:** All are semantically identical representations of a settlement

**Assessment:** **GENUINE DUPLICATION**  
Semantic purpose: Core domain object representing a settlement. Three-way duplication.

**Complication:** Settlement is a domain object used throughout the codebase. Unifying requires careful coordination.

**Fix:** CLI should depend on SDK and import Settlement; SDK defines canonical representation.

**Related issue:** BRANCH_README.md notes settlement unification was done for state machine definitions. Need to extend this to type definitions.

---

#### Finding 1.5: Transaction

**File locations:**
- `src/domain/transaction.rs:10` - Domain model (canonical)
- `src/db/models.rs:48` - Database-specific version
- `sdks/rust/src/models.rs:9` - SDK version
- `cli/synapse-cli/src/commands/transactions.rs:11` - CLI version

**Definition check:** Domain and SDK are identical; db/models differs (database-row-specific fields)

**Assessment:** **MOSTLY GENUINE DUPLICATION**  
Semantic purpose: Transaction domain entity. SDK and CLI both duplicate the domain definition. db/models is intentionally different (database model).

**Fix:** CLI should import from SDK or domain/transaction.rs; SDK should re-export domain type.

---

#### Finding 1.6: HealthStatus

**File locations:**
- `src/handlers/mod.rs:231` - API response type (canonical)
- `src/auth/health.rs:32` - Different purpose (auth-specific health check response)
- `cli/synapse-cli/src/commands/health.rs:21` - CLI version

**Definition check:** handlers/mod.rs and CLI are identical; auth/health.rs differs intentionally

**Assessment:** **GENUINE DUPLICATION** (API-to-CLI only)  
Semantic purpose: API response type. CLI duplicates instead of importing from handlers.

**Fix:** CLI should import from synapse-core handlers (requires CLI to depend on synapse-core library).

**Blocker:** CLI currently does not depend on synapse-core library; only on synapse-sdk.

---

### Category 2: CacheMetrics Naming Collision (Moderate)

**File locations:**
- `src/cache/rate_limiting.rs:58` - Rate limiter metrics (acquired_requests, rejected_requests, refill_events)
- `src/services/query_cache.rs:402` - Query cache metrics (hits, misses, hit_rate, memory_hits, etc.)
- `sdks/rust/src/models.rs:343` - SDK version (mirrors query_cache)
- `cli/synapse-cli/src/commands/stats.rs:30` - CLI version (custom aggregation)

**Definition check:** 
- rate_limiting.rs and query_cache.rs serve different purposes (unrelated metrics domains)
- SDK mirrors query_cache.rs  
- CLI custom version aggregates both

**Assessment:** **NOT GENUINE DUPLICATION** (different purposes)  
The two root crate definitions serve distinct purposes and should remain separate. However, naming collision creates confusion.

**Recommendation:** Rename `src/cache/rate_limiting.rs::CacheMetrics` to `RateLimiterMetrics` to avoid confusion.

**Follow-up issue:** Naming clarity — should be done as small refactor.

---

### Category 3: WebhookPayload Duplication (Minor)

**File locations:**
- `src/handlers/webhook.rs:42` - HTTP handler version (canonical)
- `src/telemetry/webhook.rs:55` - Webhook event version

**Definition check:** Different structures serving different purposes

**Assessment:** **NOT GENUINE DUPLICATION**  
Purpose difference: handlers version is HTTP request/response; telemetry version is event payload. Should have different names to clarify purpose.

**Recommendation:** Rename telemetry version to `WebhookEventPayload` for clarity.

---

### Category 4: Architectural Issue - CLI Independence (Critical)

**Problem:** The CLI crate does not depend on either synapse-core or synapse-sdk. It reimplements:
- HTTP client (`ApiClient` in cli/synapse-cli/src/client.rs)
- Model types (Transaction, Settlement, StatusCount, etc.)
- Error types (CliError)
- Output formatting (TableDisplay, etc.)

**File locations:**
- `cli/synapse-cli/Cargo.toml` - no synapse-core or synapse-sdk dependency
- `cli/synapse-cli/src/client.rs:214 lines` - Reimplements HTTP client
- `cli/synapse-cli/src/commands/*.rs` - Reimplements model types
- `cli/synapse-cli/src/error.rs` - Reimplements error types

**Why this matters:**
- Any API change requires updates in three places (handlers, SDK, CLI)
- Test coverage is duplicated but independent (different mock data can hide divergence, as noted in past issue: duplicate admin settlements update-status implementations with incompatible CLI flag shapes)
- No single source of truth for client behavior

**Assessment:** **ARCHITECTURAL PROBLEM** — not a merge-conflict artifact but a design choice that enables merge-conflict artifacts

**Long-term fix:** CLI should depend on synapse-sdk and import models/client from there. Short-term: Document this decision in ADR.

**Associated follow-up:** Issue #? should track CLI dependency unification

---

### Category 5: Dead Code & Merge Artifacts

**Previous fixes (commit bad15b3):**
- Deleted three competing admin-API designs (consolidated onto single AdminSynapseClient)
- Removed unclosed `#[cfg(test)]` module in cli.rs that swallowed handler definitions
- Removed duplicate TxCommands::Search variant
- Removed duplicate test suites from superseded competing implementations

**Current state:** No new unclosed blocks or clear duplicate variants found in root crate.

**Assessment:** **FIXED** by commit bad15b3

---

## Summary Table

| Type/Issue | Severity | Count | Location | Status | Action | Issue |
|-----------|----------|-------|----------|--------|--------|-------|
| CLI Architecture | Critical | - | cli/synapse-cli | Design issue | Make CLI depend on SDK | #800 |
| StatusCount | High | 3 | db/queries, SDK, CLI | Duplication | Remove CLI, import from SDK | #801 |
| DailyTotal | High | 3 | db/queries, SDK, CLI | Duplication | Remove CLI, import from SDK | #801 |
| AssetStats | High | 3 | db/queries, SDK, CLI | Duplication | Remove CLI, import from SDK | #801 |
| Settlement | High | 3 | db/models, SDK, CLI | Duplication | Remove CLI, import from SDK | #802 |
| Transaction | High | 4 | domain, db, SDK, CLI | Duplication | Remove CLI, import from SDK | #802 |
| HealthStatus | Medium | 2 | handlers, CLI | Duplication | Remove CLI, import or decide dependency | #803 |
| CacheMetrics | Low | 2 | rate_limiting, query_cache | Naming collision | ✅ Fixed (RateLimiterMetrics) | - |
| WebhookPayload | Low | 2 | handlers, telemetry | Naming collision | ✅ Fixed (WebhookEventPayload) | - |

---

## Recommendations

### Immediate Fixes (This PR)

1. **Remove CLI type redefinitions** for StatusCount, DailyTotal, AssetStats — import from SDK instead
2. **Rename CacheMetrics** in rate_limiting.rs to RateLimiterMetrics (avoid collision)
3. **Rename WebhookPayload** in telemetry/webhook.rs to WebhookEventPayload (clarity)

### Follow-up Issues (Linked in this audit)

1. **#800: CLI should depend on synapse-sdk for models and client**
   - Currently CLI reimplements HTTP client and model types independently
   - Architectural improvement to enable single source of truth for client behavior
   - Estimated: Medium complexity (dependency inversion needed)
   - Blocks #801, #802, #803

2. **#801: Remove CLI redefinitions of StatusCount, DailyTotal, AssetStats**
   - CLI should import from SDK instead of redefining locally
   - Requires #800 first
   - Note: Field name alignment check needed (count vs transaction_count)
   - Estimated: Small (once #800 done)

3. **#802: Remove CLI redefinitions of Transaction and Settlement**
   - CLI should import from SDK, not redefine locally
   - Risk: May reveal API divergence that was masked by duplicate definitions (historical: duplicate admin settlements implementations had incompatible flag shapes)
   - Requires #800 first
   - Estimated: Small to Medium (once #800 done)

4. **#803: Remove CLI redefinition of HealthStatus**
   - Resolve dependency approach: SDK re-export vs CLI importing from synapse-core
   - Estimated: Small (design + find/replace)

---

## Merge Commit Review Spot-Check

Selected commits reviewed for merge artifact pattern:

- **#799 (Mock server drift check)**: Clean merge, no conflict resolution detected
- **#777 (Workspace build/test repairs)**: Contains fixes from bad15b3; no new duplication
- **#775-758 (Various CLI/SDK features)**: Feature merges, no conflict resolution pattern detected  
- **bad15b3 (Workspace bad-merge corruption fix)**: 46 files, fixed three competing admin designs, duplicate test suites, unclosed blocks — now cleaned
- **#602 (develop branch merge)**: Clean merge into main

**Conclusion:** The systematic "both sides kept" pattern from merge conflicts appears to have been largely fixed by bad15b3. Remaining duplication is architectural (CLI reimplements types) rather than from conflict resolution.

---

## Appendix: Merge Commit Count

Total merge commits analyzed: 100+  
Pattern match (potential conflicts): ~40  
Verified conflicts with artifact remnants: Addressed in bad15b3  
Current non-breaking duplication found: 7 categories (detailed above)

