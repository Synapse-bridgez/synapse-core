# Database Failover Architecture Diagram

## System Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        Synapse Core API                          │
│                                                                   │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │                       AppState                              │ │
│  │  ┌──────────────────────────────────────────────────────┐  │ │
│  │  │              PoolManager                              │  │ │
│  │  │                                                        │  │ │
│  │  │  ┌──────────────────┐      ┌──────────────────┐     │  │ │
│  │  │  │  Primary Pool    │      │  Replica Pool    │     │  │ │
│  │  │  │  (Read-Write)    │      │  (Read-Only)     │     │  │ │
│  │  │  │  Max: 10 conns   │      │  Max: 10 conns   │     │  │ │
│  │  │  │  Arc<PgPool>     │      │  Arc<PgPool>     │     │  │ │
│  │  │  └────────┬─────────┘      └────────┬─────────┘     │  │ │
│  │  │           │                         │               │  │ │
│  │  │           │  QueryIntent Router     │               │  │ │
│  │  │           │  ┌──────────────────┐   │               │  │ │
│  │  │           │  │ Write → Primary  │   │               │  │ │
│  │  │           │  │ Read  → Replica  │   │               │  │ │
│  │  │           │  └──────────────────┘   │               │  │ │
│  │  └───────────┼──────────────────────────┼───────────────┘  │ │
│  └──────────────┼──────────────────────────┼──────────────────┘ │
└─────────────────┼──────────────────────────┼────────────────────┘
                  │                          │
                  │                          │
         ┌────────▼────────┐        ┌────────▼────────┐
         │                 │        │                 │
         │  Primary DB     │◄───────┤  Replica DB     │
         │  (us-east-1)    │ Repl.  │  (us-west-2)    │
         │                 │        │                 │
         │  Read + Write   │        │  Read Only      │
         └─────────────────┘        └─────────────────┘
```

## Query Flow

### Read Query (SELECT)
```
Client Request
     │
     ▼
┌─────────────────┐
│  Handler        │
│  (get_tx)       │
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  Query Function │
│  QueryIntent::  │
│  Read           │
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  PoolManager    │
│  .get_pool()    │
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  Replica Pool   │
│  (or Primary    │
│   if no replica)│
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  Replica DB     │
│  Execute Query  │
└────────┬────────┘
         │
         ▼
    Response
```

### Write Query (INSERT/UPDATE)
```
Client Request
     │
     ▼
┌─────────────────┐
│  Handler        │
│  (insert_tx)    │
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  Query Function │
│  QueryIntent::  │
│  Write          │
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  PoolManager    │
│  .get_pool()    │
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  Primary Pool   │
│  (always)       │
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  Primary DB     │
│  Execute Query  │
└────────┬────────┘
         │
         ▼
    Response
```

## Connection Recovery Flow

```
Connection Lost
     │
     ▼
┌─────────────────────────────────┐
│  Detect Connection Failure      │
└────────┬────────────────────────┘
         │
         ▼
┌─────────────────────────────────┐
│  Attempt 1: Retry after 2s      │
└────────┬────────────────────────┘
         │
         ├─Success──► Connected
         │
         ▼ Failure
┌─────────────────────────────────┐
│  Attempt 2: Retry after 4s      │
└────────┬────────────────────────┘
         │
         ├─Success──► Connected
         │
         ▼ Failure
┌─────────────────────────────────┐
│  Attempt 3: Retry after 8s      │
└────────┬────────────────────────┘
         │
         ├─Success──► Connected
         │
         ▼ Failure
┌─────────────────────────────────┐
│  Attempt 4: Retry after 16s     │
└────────┬────────────────────────┘
         │
         ├─Success──► Connected
         │
         ▼ Failure
┌─────────────────────────────────┐
│  Attempt 5: Retry after 32s     │
└────────┬────────────────────────┘
         │
         ├─Success──► Connected
         │
         ▼ Failure
┌─────────────────────────────────┐
│  Mark Connection Failed         │
│  Log Error                      │
│  Retry on Next Query            │
└─────────────────────────────────┘
```

## Health Check Flow

```
GET /health
     │
     ▼
┌─────────────────────────────────┐
│  Health Handler                 │
└────────┬────────────────────────┘
         │
         ▼
┌─────────────────────────────────┐
│  pool_manager.health_check()    │
└────────┬────────────────────────┘
         │
         ├──────────────────┐
         │                  │
         ▼                  ▼
┌──────────────────┐  ┌──────────────────┐
│  Check Primary   │  │  Check Replica   │
│  SELECT 1        │  │  SELECT 1        │
└────────┬─────────┘  └────────┬─────────┘
         │                     │
         ▼                     ▼
    Connected?            Connected?
         │                     │
         └──────────┬──────────┘
                    │
                    ▼
         ┌─────────────────────┐
         │  Build Response     │
         │  {                  │
         │    primary: bool,   │
         │    replica: bool    │
         │  }                  │
         └──────────┬──────────┘
                    │
                    ▼
         ┌─────────────────────┐
         │  Return Status      │
         │  200 if all OK      │
         │  503 if any down    │
         └─────────────────────┘
```

## Configuration Modes

### Mode 1: Single Database (Backward Compatible)
```
.env:
  DATABASE_URL=postgres://primary:5432/synapse

Result:
  ┌──────────────┐
  │ PoolManager  │
  │              │
  │  Primary: ✓  │
  │  Replica: ✗  │
  │              │
  │  All queries │
  │  → Primary   │
  └──────────────┘
```

### Mode 2: Primary + Replica (High Availability)
```
.env:
  DATABASE_URL=postgres://primary:5432/synapse
  DATABASE_REPLICA_URL=postgres://replica:5432/synapse

Result:
  ┌──────────────┐
  │ PoolManager  │
  │              │
  │  Primary: ✓  │
  │  Replica: ✓  │
  │              │
  │  Reads  → Replica │
  │  Writes → Primary │
  └──────────────┘
```

## Load Distribution

### Before (Single Database)
```
┌─────────────────────────────────┐
│         Primary DB              │
│                                 │
│  Reads:  70% ████████████████   │
│  Writes: 30% ██████             │
│                                 │
│  Total Load: 100%               │
└─────────────────────────────────┘
```

### After (With Replica)
```
┌─────────────────────────────────┐
│         Primary DB              │
│                                 │
│  Writes: 30% ██████             │
│                                 │
│  Total Load: 30%                │
└─────────────────────────────────┘

┌─────────────────────────────────┐
│         Replica DB              │
│                                 │
│  Reads:  70% ████████████████   │
│                                 │
│  Total Load: 70%                │
└─────────────────────────────────┘
```

## Failure Scenarios

### Scenario 1: Replica Fails
```
┌──────────────┐     ┌──────────────┐
│   Primary    │     │   Replica    │
│   ✓ UP       │     │   ✗ DOWN     │
└──────────────┘     └──────────────┘
        │
        ▼
┌─────────────────────────────────┐
│  Automatic Fallback             │
│  All queries → Primary          │
│  Service continues normally     │
│  Health check returns 503       │
└─────────────────────────────────┘
```

### Scenario 2: Primary Fails
```
┌──────────────┐     ┌──────────────┐
│   Primary    │     │   Replica    │
│   ✗ DOWN     │     │   ✓ UP       │
└──────────────┘     └──────────────┘
        │
        ▼
┌─────────────────────────────────┐
│  Service Degraded               │
│  Reads continue → Replica       │
│  Writes fail (expected)         │
│  Health check returns 503       │
│  Manual failover required       │
└─────────────────────────────────┘
```

### Scenario 3: Both Fail
```
┌──────────────┐     ┌──────────────┐
│   Primary    │     │   Replica    │
│   ✗ DOWN     │     │   ✗ DOWN     │
└──────────────┘     └──────────────┘
        │
        ▼
┌─────────────────────────────────┐
│  Service Unavailable            │
│  All queries fail               │
│  Health check returns 503       │
│  Automatic retry with backoff   │
└─────────────────────────────────┘
```

## Thread Safety

```
┌─────────────────────────────────────────┐
│           PoolManager (Clone)           │
│                                         │
│  ┌─────────────────────────────────┐   │
│  │     Arc<PgPool> (Primary)       │   │
│  │  ┌───────────────────────────┐  │   │
│  │  │   Shared across threads   │  │   │
│  │  │   Thread-safe reference   │  │   │
│  │  └───────────────────────────┘  │   │
│  └─────────────────────────────────┘   │
│                                         │
│  ┌─────────────────────────────────┐   │
│  │  Option<Arc<PgPool>> (Replica)  │   │
│  │  ┌───────────────────────────┐  │   │
│  │  │   Shared across threads   │  │   │
│  │  │   Thread-safe reference   │  │   │
│  │  └───────────────────────────┘  │   │
│  └─────────────────────────────────┘   │
└─────────────────────────────────────────┘
         │              │              │
         ▼              ▼              ▼
    Thread 1       Thread 2       Thread 3
    (Handler)      (Handler)      (Handler)
```

## Deployment Timeline

```
Time: 0min          5min          10min         15min
  │                 │             │             │
  ▼                 ▼             ▼             ▼
Deploy          Verify        Monitor       Stable
Code            Health        Metrics       State
  │                 │             │             │
  ├─Build           ├─Check       ├─CPU         ├─All OK
  ├─Test            ├─Logs        ├─Memory      ├─Replica
  ├─Deploy          ├─Queries     ├─Latency     │  Active
  └─Restart         └─Status      └─Errors      └─Primary
                                                   Reduced
```

---

## Legend

```
✓ = Healthy/Active
✗ = Failed/Inactive
→ = Routes to
◄─ = Replicates from
│ = Flow direction
┌─┐ = Component boundary
```

## Quick Reference

| Symbol | Meaning |
|--------|---------|
| Primary Pool | Read-write connection pool to primary DB |
| Replica Pool | Read-only connection pool to replica DB |
| QueryIntent::Read | Routes to replica (or primary if no replica) |
| QueryIntent::Write | Always routes to primary |
| Arc<PgPool> | Thread-safe shared connection pool |
| Health Check | Monitors both primary and replica status |

---

**See also**:
- Full documentation: `docs/database_failover.md`
- Quick reference: `DB_FAILOVER_QUICK_REF.md`
- Implementation: `src/db/pool_manager.rs`
