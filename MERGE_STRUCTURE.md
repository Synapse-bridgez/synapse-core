# Synapse Core — Merge Structure

> **Purpose:** This document defines the merge order for all 129 issues.
> Issues within the same wave can be worked on in parallel but must be **merged sequentially** within their conflict group.
> Issues in different conflict groups within the same wave can be merged in any order.

---

## Hot File Map — Conflict Risk Zones

These files are touched by the most issues. **Any PR modifying these files must be rebased immediately before merge.**

| File | Issues Touching It | Risk Level |
|---|---|---|
| `src/main.rs` | 18 issues | 🔴 CRITICAL |
| `src/db/queries.rs` | 17 issues | 🔴 CRITICAL |
| `migrations/` | 16 issues | 🔴 CRITICAL |
| `src/config.rs` | 14 issues | 🔴 CRITICAL |
| `.github/workflows/rust.yml` | 12 issues | 🟠 HIGH |
| `src/handlers/webhook.rs` | 10 issues | 🟠 HIGH |
| `src/services/webhook_dispatcher.rs` | 10 issues | 🟠 HIGH |
| `src/services/processor.rs` | 9 issues | 🟠 HIGH |
| `src/lib.rs` | 8 issues | 🟡 MEDIUM |
| `Cargo.toml` | 8 issues | 🟡 MEDIUM |
| `src/db/models.rs` | 7 issues | 🟡 MEDIUM |
| `src/middleware/idempotency.rs` | 7 issues | 🟡 MEDIUM |

### Merge Rule for Hot Files

When merging a PR that touches a 🔴 CRITICAL file:
1. Merge it **first** in its wave
2. Immediately notify all other open PRs in the wave to rebase
3. Wait for rebases before merging the next PR touching the same file

---

## Wave 0 — Foundation (Merge FIRST — These Affect Everything)

> These issues change core types, module structure, or error handling patterns that most other issues depend on.
> **Merge all of Wave 0 before assigning any other wave.**

| # | Issue | Key Files | Merge Order |
|---|---|---|---|
| 104 | Refactor Transaction Status as Rust Enum | `src/db/models.rs`, `src/services/processor.rs`, `src/services/transaction_processor.rs`, `src/handlers/webhook.rs`, `src/db/queries.rs` | **1st** |
| 109 | Normalize Module Path for Multi-Tenant Directory | `src/lib.rs`, tenant dir | **2nd** |
| 106 | Eliminate Dead Code and Unused Imports | Throughout `src/` | **3rd** |
| 107 | Unify Error Handling Pattern Across All Handlers | `src/error.rs`, `src/handlers/*.rs` | **4th** |
| 108 | Add Clippy Configuration with Project-Specific Lints | `clippy.toml`, `Cargo.toml` | **5th** |

**Why first:** Issue #104 changes the `Transaction` model that 50+ issues depend on. #109 renames a module path that breaks imports. #106/#107 clean up patterns other issues would conflict with.

---

## Wave 1 — CI/CD Pipeline (No Source Code Conflicts)

> All CI issues touch only `.github/workflows/rust.yml` — merge them **sequentially** in this order.
> These have ZERO overlap with source code issues, so the wave can run alongside Wave 2.

| # | Issue | Merge Order |
|---|---|---|
| 23 | Add Redis Service Container to CI | **1st** |
| 24 | Add Cargo Clippy and Rustfmt Checks | **2nd** |
| 25 | Implement Cargo Build Caching | **3rd** |
| 26 | Add Separate CI Jobs for Unit/Integration Tests | **4th** |
| 27 | Add Security Audit Step | **5th** |
| 28 | Implement CI Matrix Testing | **6th** |
| 30 | Add Migration Validation Step | **7th** |
| 31 | Implement PR Label-Based Test Scope | **8th** |
| 32 | Add Test Coverage Reporting | **9th** |
| 85 | Blue-Green Deployment Migration Safety Check | **10th** |
| 112 | Add Changelog Generation | **11th** |
| 29 | Add Docker Image Build and Push | **12th** (also touches `dockerfile`) |

**Parallel-safe with:** Wave 2, Wave 3, Wave 6, Wave 7

---

## Wave 2 — Documentation (Zero Source Code Conflicts)

> Pure documentation — these can be merged in any order and run alongside any wave.

| # | Issue | Files | Merge Order |
|---|---|---|---|
| 86 | Comprehensive API Documentation | `docs/api-reference.md` | Any |
| 87 | Contributing Guide with ADRs | `CONTRIBUTING.md`, `docs/adr/` | Any |
| 88 | Document Disaster Recovery Procedures | `docs/disaster-recovery.md` | Any |
| 89 | Create Runbook for Common Operational Tasks | `docs/runbook.md` | Any |

**Parallel-safe with:** All waves

---

## Wave 3 — Testing Infrastructure (Mostly Independent)

> Testing files rarely overlap with production code.

| # | Issue | Files | Merge Order |
|---|---|---|---|
| 61 | Test Fixture Factory | `tests/fixtures.rs` | Any |
| 63 | Integration Test Harness | `tests/common/mod.rs` | Any |
| 64 | Load Testing with k6 | `tests/load/*.js` | Any |
| 65 | End-to-End Lifecycle Test | `tests/lifecycle_test.rs` | Any |
| 66 | Benchmark Tests | `benches/` , `Cargo.toml` | Any |
| 62 | Property-Based Testing | `src/validation/mod.rs`, `Cargo.toml` | After #66 (both touch `Cargo.toml`) |

**Parallel-safe with:** Waves 1, 2, 4+

---

## Wave 4 — Config & Infrastructure Layer

> These issues add config fields, pool settings, and startup behavior. They share `src/config.rs` and `src/main.rs`.
> **Merge sequentially within each conflict group.**

### Group A: Config File (`src/config.rs`)
Merge in this order — each adds new config fields to the same file:

| Order | # | Issue |
|---|---|---|
| 1st | 83 | Configuration Validation on Startup |
| 2nd | 84 | Environment-Specific Configuration Profiles |
| 3rd | 9 | Connection Pool Autoscaling |
| 4th | 34 | Query Timeout Configuration |
| 5th | 53 | CORS Configuration |

### Group B: Startup / `src/main.rs`
Merge in this order — each adds startup tasks or background workers:

| Order | # | Issue |
|---|---|---|
| 1st | 36 | Database Connection Pool Warm-Up on Startup |
| 2nd | 49 | Startup Readiness Probe |
| 3rd | 56 | Graceful Shutdown with Request Draining |
| 4th | 59 | Panic Recovery Middleware |
| 5th | 52 | Request Body Size Limits |
| 6th | 103 | Connection Pool Metrics Emission |

### Group C: Developer Tooling (Independent of A & B)

| # | Issue | Merge Order |
|---|---|---|
| 111 | `cargo xtask` for Development Commands | Any |
| 113 | Developer Setup Script | Any |
| 110 | CLI Seed Data Command | Any |
| 82 | Docker Compose Dev Environment with Hot Reload | Any |
| 81 | Docker Health Check | Any |
| 128 | Automated Dependency Update Workflow | Any |

**Groups A, B, C can run in parallel** — they touch different primary files.

---

## Wave 5 — Database & Query Layer

> Heavy overlap on `src/db/queries.rs` and `migrations/`. **Strict sequential merge required.**

### Group A: Query Infrastructure (merge first)

| Order | # | Issue |
|---|---|---|
| 1st | 105 | Extract Typed Query Builder | 
| 2nd | 35 | Optimize Search with Materialized Indexes |
| 3rd | 37 | Partition Pruning Optimization |
| 4th | 48 | Slow Query Detection and Alerting |
| 5th | 38 | Async-Safe Query Timeout with Cancellation |
| 6th | 57 | Retry Logic with Jitter for Transient DB Errors |

### Group B: Multi-Tenant Data Layer (merge after Group A)

| Order | # | Issue |
|---|---|---|
| 1st | 67 | Row-Level Security for Multi-Tenant Isolation |
| 2nd | 69 | Tenant Configuration Hot-Reload |
| 3rd | 68 | Tenant-Specific Rate Limiting |

### Group C: Audit & Compliance (merge after Group A)

| Order | # | Issue |
|---|---|---|
| 1st | 121 | Audit Log Search and Export |
| 2nd | 122 | Audit Log Retention Policy |
| 3rd | 123 | Compliance Report Generation |

### Group D: Read/Write Routing (independent within wave)

| # | Issue |
|---|---|
| 33 | Read-Replica Routing for Query Endpoints |
| 18 | Circuit Breaker for PostgreSQL Operations |

**Groups B, C, D** can run in parallel after Group A completes.

---

## Wave 6 — Idempotency System

> All 7 issues touch `src/middleware/idempotency.rs`. **Strictly sequential.**

| Order | # | Issue |
|---|---|---|
| 1st | 14 | Idempotency Key Validation and Normalization |
| 2nd | 12 | Idempotency Key Scoping Per Tenant |
| 3rd | 13 | Idempotency Response Body Caching |
| 4th | 15 | Idempotency Lock Timeout Recovery |
| 5th | 11 | Database-Backed Idempotency Fallback |
| 6th | 16 | Idempotency Metrics Dashboard Data |
| 7th | 17 | Circuit Breaker for Redis Connections |

**Why this order:** #14 (validation) is smallest/simplest change. #12 and #13 modify the key format and response caching. #11 adds the biggest structural change (Postgres fallback). #16/#17 add instrumentation on top.

---

## Wave 7 — Callback Processor & Transaction Pipeline

> Core scalability work — heavy overlap on `src/services/processor.rs`, `src/services/transaction_processor.rs`, and `src/handlers/webhook.rs`.

### Group A: Processor Internals (strictly sequential)

| Order | # | Issue |
|---|---|---|
| 1st | 60 | Transaction Status Transition Validation |
| 2nd | 1 | Partitioned Processor with Worker Pool |
| 3rd | 2 | Adaptive Batch Sizing |
| 4th | 7 | Processing Pipeline Stages with Hooks |
| 5th | 5 | DLQ Auto-Retry with Exponential Backoff |
| 6th | 6 | Horizontal Scaling with Redis Coordination |
| 7th | 90 | Stellar Transaction Verification in Processor |
| 8th | 129 | Resource Limits to Background Tasks |

### Group B: Callback Ingestion (strictly sequential, can parallel with A after #60)

| Order | # | Issue |
|---|---|---|
| 1st | 4 | Priority Queue Support |
| 2nd | 3 | Back-Pressure Mechanism |
| 3rd | 8 | Batched Insert for High-Volume Ingestion |
| 4th | 50 | API Key Authentication for Callbacks |
| 5th | 73 | Transaction Filtering by Date Range |

### Group C: API Endpoints (after Group B)

| Order | # | Issue |
|---|---|---|
| 1st | 70 | API Versioning V1/V2 Route Groups |
| 2nd | 72 | Cursor-Based Pagination for All Endpoints |
| 3rd | 74 | Bulk Transaction Status Update |
| 4th | 71 | OpenAPI Specification Auto-Generation |
| 5th | 58 | Structured Error Responses with Request Context |
| 6th | 44 | Structured Logging with Correlation IDs |

---

## Wave 8 — Webhook System

> Concentrated on `src/services/webhook_dispatcher.rs`. **Sequential merge.**

| Order | # | Issue |
|---|---|---|
| 1st | 43 | Webhook Delivery Deduplication |
| 2nd | 42 | Webhook Event Filtering by Transaction Properties |
| 3rd | 40 | Webhook Payload Signing Version Support |
| 4th | 41 | Webhook Delivery Rate Limiting Per Endpoint |
| 5th | 10 | Concurrent Webhook Delivery |
| 6th | 39 | Webhook Endpoint Health Scoring |
| 7th | 21 | Circuit Breaker for Webhook Delivery |
| 8th | 102 | Optimize Webhook Dispatcher Database Queries |

**Why this order:** Dedup and filtering (#43, #42) change the query/enqueue path. Signing (#40) changes the delivery path. Rate limiting (#41) and concurrency (#10) change the execution model. Health scoring (#39) and circuit breakers (#21) add behavioral layers on top.

---

## Wave 9 — Stellar, Caching, WebSocket, Observability

> These issues form smaller clusters with limited cross-overlap.

### Group A: Stellar Integration (sequential — share `src/stellar/client.rs`)

| Order | # | Issue |
|---|---|---|
| 1st | 19 | Half-Open State Monitoring for Horizon CB |
| 2nd | 20 | Circuit Breaker State Persistence |
| 3rd | 93 | Horizon API Response Caching |
| 4th | 91 | Stellar Transaction Streaming (SSE) |
| 5th | 92 | Multi-Asset Support |

### Group B: Caching (sequential — share `src/services/query_cache.rs`)

| Order | # | Issue |
|---|---|---|
| 1st | 124 | Cache Stampede Prevention |
| 2nd | 125 | Cache Key Namespacing for Multi-Tenant |
| 3rd | 101 | In-Memory LRU Cache Layer |
| 4th | 126 | Cache Warming on Partition Rotation |

### Group C: WebSocket (sequential — share `src/handlers/ws.rs`)

| Order | # | Issue |
|---|---|---|
| 1st | 114 | WebSocket Authentication |
| 2nd | 115 | WebSocket Heartbeat and Health Monitoring |
| 3rd | 116 | WebSocket Message Backpressure |
| 4th | 75 | GraphQL Subscription for Real-Time Updates |

### Group D: Observability (sequential where noted)

| Order | # | Issue |
|---|---|---|
| 1st | 45 | OpenTelemetry Metrics Exporter |
| 2nd | 46 | Prometheus-Compatible Metrics Endpoint |
| 3rd | 47 | Health Check Severity Levels |
| 4th | 127 | Request Tracing Through Full Pipeline |

### Group E: Independent (any order)

| # | Issue |
|---|---|
| 22 | Cascading Circuit Breaker Dashboard |
| 117 | Distributed Lock Timeout Monitoring |
| 118 | Fair Lock Queuing for Worker Coordination |
| 100 | Query Result Streaming for Large Exports |

**Groups A–E can all run in parallel.**

---

## Wave 10 — Final Layer (Depends on Earlier Waves)

> These issues build on features from previous waves.

### Group A: Settlement & Reconciliation

| Order | # | Issue | Depends On |
|---|---|---|---|
| 1st | 79 | Settlement Batch Size Limits | Wave 0 (#104) |
| 2nd | 76 | Settlement Dispute Resolution | Wave 0 (#104) |
| 3rd | 77 | Automated Reconciliation Scheduling | Wave 4 (#103) |
| 4th | 78 | Reconciliation Report API | #77 |

### Group B: Security Hardening

| Order | # | Issue | Depends On |
|---|---|---|---|
| 1st | 51 | Rate Limiting Per Tenant | Wave 6 (#12, #17) |
| 2nd | 54 | Secrets Rotation Without Downtime | Wave 4 (#83) |
| 3rd | 55 | Input Sanitization for GraphQL | Wave 9C (#75) |
| 4th | 80 | Rolling Restart with Connection Draining | Wave 4 (#56) |

### Group C: Asset Management

| Order | # | Issue | Depends On |
|---|---|---|---|
| 1st | 119 | Asset Registry with Issuer Validation | Wave 0 (#104) |
| 2nd | 120 | Asset-Level Configuration for Processing Rules | #119 |

### Group D: Feature Flags (sequential — share `src/services/feature_flags.rs`)

| Order | # | Issue |
|---|---|---|
| 1st | 97 | Feature Flag Percentage Rollout |
| 2nd | 98 | Feature Flag Audit Trail |
| 3rd | 99 | Feature Flag Dependencies |

### Group E: Backup & Recovery

| Order | # | Issue |
|---|---|---|
| 1st | 94 | Point-in-Time Recovery Support |
| 2nd | 95 | Backup Verification via Automated Restore |
| 3rd | 96 | Backup Progress Reporting |

---

## Visual Merge Flow

```
Wave 0  ████████████████████  FOUNDATION (must complete first)
         │
         ├─── Wave 1  ██████████████  CI/CD (sequential within)
         │
         ├─── Wave 2  ████  DOCS (any order)
         │
         ├─── Wave 3  ██████  TESTING (any order)
         │
         ├─── Wave 4  ██████████████  CONFIG & INFRA
         │     ├── Group A (config.rs — sequential)
         │     ├── Group B (main.rs — sequential)
         │     └── Group C (dev tooling — any order)
         │
         ├─── Wave 5  ████████████████████  DATABASE
         │     ├── Group A (query infra — sequential, FIRST)
         │     ├── Group B (multi-tenant — after A)
         │     ├── Group C (audit — after A)
         │     └── Group D (routing — after A)
         │
         ├─── Wave 6  ██████████████  IDEMPOTENCY (strictly sequential)
         │
         ├─── Wave 7  ████████████████████████  PROCESSOR & API
         │     ├── Group A (processor — sequential)
         │     ├── Group B (ingestion — sequential)
         │     └── Group C (API endpoints — sequential)
         │
         ├─── Wave 8  ████████████████  WEBHOOKS (strictly sequential)
         │
         ├─── Wave 9  ████████████████████  STELLAR/CACHE/WS/OBSERVABILITY
         │     ├── Group A (Stellar —  sequential)
         │     ├── Group B (Caching — sequential)
         │     ├── Group C (WebSocket — sequential)
         │     ├── Group D (Observability — sequential)
         │     └── Group E (independent — any order)
         │
         └─── Wave 10 ████████████████████  FINAL LAYER
               ├── Group A (Settlement)
               ├── Group B (Security)
               ├── Group C (Assets)
               ├── Group D (Feature Flags)
               └── Group E (Backup)
```

**Waves 1–3 can run simultaneously.**
**Waves 4–9 can run simultaneously** (groups within each wave are sequential).
**Wave 10 runs last.**

---

## Merge Checklist for Maintainers

When merging any PR:

1. ✅ CI passes (fmt, clippy, build, test)
2. ✅ PR targets `develop` branch
3. ✅ PR is rebased on latest `develop` (no merge commits)
4. ✅ Check this document — is this issue's wave/group predecessor already merged?
5. ✅ If PR touches a 🔴 CRITICAL file, notify all open PRs in the same wave to rebase
6. ✅ Squash-merge to keep history clean
7. ✅ After merge, comment on the next issue in the group: "Predecessor merged — please rebase"

---

## Quick Reference: Issue → Wave Lookup

| Issue # | Wave | Group | Position |
|---|---|---|---|
| 1 | 7 | A | 2nd |
| 2 | 7 | A | 3rd |
| 3 | 7 | B | 2nd |
| 4 | 7 | B | 1st |
| 5 | 7 | A | 5th |
| 6 | 7 | A | 6th |
| 7 | 7 | A | 4th |
| 8 | 7 | B | 3rd |
| 9 | 4 | A | 3rd |
| 10 | 8 | — | 5th |
| 11 | 6 | — | 5th |
| 12 | 6 | — | 2nd |
| 13 | 6 | — | 3rd |
| 14 | 6 | — | 1st |
| 15 | 6 | — | 4th |
| 16 | 6 | — | 6th |
| 17 | 6 | — | 7th |
| 18 | 5 | D | — |
| 19 | 9 | A | 1st |
| 20 | 9 | A | 2nd |
| 21 | 8 | — | 7th |
| 22 | 9 | E | — |
| 23 | 1 | — | 1st |
| 24 | 1 | — | 2nd |
| 25 | 1 | — | 3rd |
| 26 | 1 | — | 4th |
| 27 | 1 | — | 5th |
| 28 | 1 | — | 6th |
| 29 | 1 | — | 12th |
| 30 | 1 | — | 7th |
| 31 | 1 | — | 8th |
| 32 | 1 | — | 9th |
| 33 | 5 | D | — |
| 34 | 4 | A | 4th |
| 35 | 5 | A | 2nd |
| 36 | 4 | B | 1st |
| 37 | 5 | A | 3rd |
| 38 | 5 | A | 5th |
| 39 | 8 | — | 6th |
| 40 | 8 | — | 3rd |
| 41 | 8 | — | 4th |
| 42 | 8 | — | 2nd |
| 43 | 8 | — | 1st |
| 44 | 7 | C | 6th |
| 45 | 9 | D | 1st |
| 46 | 9 | D | 2nd |
| 47 | 9 | D | 3rd |
| 48 | 5 | A | 4th |
| 49 | 4 | B | 2nd |
| 50 | 7 | B | 4th |
| 51 | 10 | B | 1st |
| 52 | 4 | B | 5th |
| 53 | 4 | A | 5th |
| 54 | 10 | B | 2nd |
| 55 | 10 | B | 3rd |
| 56 | 4 | B | 3rd |
| 57 | 5 | A | 6th |
| 58 | 7 | C | 5th |
| 59 | 4 | B | 4th |
| 60 | 7 | A | 1st |
| 61 | 3 | — | Any |
| 62 | 3 | — | Last |
| 63 | 3 | — | Any |
| 64 | 3 | — | Any |
| 65 | 3 | — | Any |
| 66 | 3 | — | Any |
| 67 | 5 | B | 1st |
| 68 | 5 | B | 3rd |
| 69 | 5 | B | 2nd |
| 70 | 7 | C | 1st |
| 71 | 7 | C | 4th |
| 72 | 7 | C | 2nd |
| 73 | 7 | B | 5th |
| 74 | 7 | C | 3rd |
| 75 | 9 | C | 4th |
| 76 | 10 | A | 2nd |
| 77 | 10 | A | 3rd |
| 78 | 10 | A | 4th |
| 79 | 10 | A | 1st |
| 80 | 10 | B | 4th |
| 81 | 4 | C | Any |
| 82 | 4 | C | Any |
| 83 | 4 | A | 1st |
| 84 | 4 | A | 2nd |
| 85 | 1 | — | 10th |
| 86 | 2 | — | Any |
| 87 | 2 | — | Any |
| 88 | 2 | — | Any |
| 89 | 2 | — | Any |
| 90 | 7 | A | 7th |
| 91 | 9 | A | 4th |
| 92 | 9 | A | 5th |
| 93 | 9 | A | 3rd |
| 94 | 10 | E | 1st |
| 95 | 10 | E | 2nd |
| 96 | 10 | E | 3rd |
| 97 | 10 | D | 1st |
| 98 | 10 | D | 2nd |
| 99 | 10 | D | 3rd |
| 100 | 9 | E | — |
| 101 | 9 | B | 3rd |
| 102 | 8 | — | 8th |
| 103 | 4 | B | 6th |
| 104 | 0 | — | 1st |
| 105 | 5 | A | 1st |
| 106 | 0 | — | 3rd |
| 107 | 0 | — | 4th |
| 108 | 0 | — | 5th |
| 109 | 0 | — | 2nd |
| 110 | 4 | C | Any |
| 111 | 4 | C | Any |
| 112 | 1 | — | 11th |
| 113 | 4 | C | Any |
| 114 | 9 | C | 1st |
| 115 | 9 | C | 2nd |
| 116 | 9 | C | 3rd |
| 117 | 9 | E | — |
| 118 | 9 | E | — |
| 119 | 10 | C | 1st |
| 120 | 10 | C | 2nd |
| 121 | 5 | C | 1st |
| 122 | 5 | C | 2nd |
| 123 | 5 | C | 3rd |
| 124 | 9 | B | 1st |
| 125 | 9 | B | 2nd |
| 126 | 9 | B | 4th |
| 127 | 9 | D | 4th |
| 128 | 4 | C | Any |
| 129 | 7 | A | 8th |
