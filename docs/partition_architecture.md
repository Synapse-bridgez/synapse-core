# Database Partitioning Architecture

## Partition Structure

```
transactions (Parent Table - Partitioned)
│
├── transactions_y2025m02 (2025-02-01 to 2025-03-01)
│   ├── Inherits: All indexes from parent
│   ├── Contains: Records with created_at in Feb 2025
│   └── Size: ~X MB
│
├── transactions_y2025m03 (2025-03-01 to 2025-04-01)
│   ├── Inherits: All indexes from parent
│   ├── Contains: Records with created_at in Mar 2025
│   └── Size: ~X MB
│
├── transactions_y2025m04 (2025-04-01 to 2025-05-01)
│   ├── Inherits: All indexes from parent
│   ├── Contains: Records with created_at in Apr 2025
│   └── Size: ~X MB
│
└── ... (Future partitions auto-created)
```

## Data Flow

```
INSERT INTO transactions
        ↓
[Partition Router]
        ↓
Checks created_at value
        ↓
Routes to appropriate partition
        ↓
transactions_y2025m02 (if Feb 2025)
```

## Query Optimization (Partition Pruning)

```
SELECT * FROM transactions 
WHERE created_at >= '2025-02-01' 
  AND created_at < '2025-03-01'

        ↓
[Query Planner]
        ↓
Analyzes WHERE clause
        ↓
Prunes irrelevant partitions
        ↓
Scans ONLY transactions_y2025m02
(Ignores all other partitions)
```

## Maintenance Workflow

```
PartitionManager (Background Task)
        ↓
Every 24 hours
        ↓
maintain_partitions()
        ↓
    ┌───────────────┴───────────────┐
    ↓                               ↓
create_monthly_partition()    detach_old_partitions(12)
    ↓                               ↓
Creates partition for         Detaches partitions
2 months ahead                older than 12 months
    ↓                               ↓
transactions_y2025m06         transactions_y2024m01
(if doesn't exist)            (becomes standalone table)
```

## Lifecycle of a Partition

```
1. CREATION (2 months before needed)
   └─> create_monthly_partition()
       └─> transactions_y2025m06 created

2. ACTIVE (Current month)
   └─> Receives INSERT/UPDATE/DELETE operations
       └─> Indexed and optimized

3. AGING (1-12 months old)
   └─> Read-only queries
       └─> Occasional VACUUM

4. DETACHMENT (After 12 months)
   └─> detach_old_partitions(12)
       └─> Becomes standalone table

5. ARCHIVAL (Manual)
   └─> Export to CSV / Move to archive schema
       └─> DROP TABLE (after backup)
```

## Index Inheritance

```
Parent Table: transactions
├── idx_transactions_status
├── idx_transactions_stellar_account
└── idx_transactions_created_at

        ↓ (Inherited by all partitions)

transactions_y2025m02
├── transactions_y2025m02_status_idx
├── transactions_y2025m02_stellar_account_idx
└── transactions_y2025m02_created_at_idx

transactions_y2025m03
├── transactions_y2025m03_status_idx
├── transactions_y2025m03_stellar_account_idx
└── transactions_y2025m03_created_at_idx
```

## Storage Layout

```
PostgreSQL Data Directory
└── base/
    └── {database_oid}/
        ├── transactions (parent - metadata only)
        ├── transactions_y2025m02 (actual data)
        ├── transactions_y2025m03 (actual data)
        ├── transactions_y2025m04 (actual data)
        └── transactions_old (backup - can be dropped)
```

## Performance Comparison

### Before Partitioning
```
SELECT * FROM transactions WHERE created_at = '2025-02-15'
└─> Seq Scan on transactions (cost=0..10000 rows=1000000)
    └─> Scans ALL 1,000,000 rows
```

### After Partitioning
```
SELECT * FROM transactions WHERE created_at = '2025-02-15'
└─> Seq Scan on transactions_y2025m02 (cost=0..100 rows=30000)
    └─> Scans ONLY 30,000 rows (Feb partition)
    └─> 97% reduction in rows scanned!
```

## Retention Policy

```
Timeline (12-month retention):
─────────────────────────────────────────────────────────────>
                                                        Time

[Detached]  [Active Partitions (12 months)]  [Future]
    ↓              ↓                              ↓
2024-01      2024-02 ... 2025-02           2025-03, 2025-04
(archived)   (queryable)                   (pre-created)
```

## Monitoring Dashboard (Conceptual)

```
┌─────────────────────────────────────────────────┐
│ Partition Health Dashboard                     │
├─────────────────────────────────────────────────┤
│ Total Partitions: 15                            │
│ Active Partitions: 12                           │
│ Detached Partitions: 3                          │
│                                                 │
│ Partition Sizes:                                │
│ ├─ transactions_y2025m02: 1.2 GB (45K rows)    │
│ ├─ transactions_y2025m03: 980 MB (38K rows)    │
│ └─ transactions_y2025m04: 0 MB (0 rows)        │
│                                                 │
│ Next Maintenance: 2025-02-18 00:00:00          │
│ Last Maintenance: 2025-02-17 00:00:00 ✓        │
└─────────────────────────────────────────────────┘
```

## Key Concepts

1. **Partition Pruning**: Query planner automatically excludes irrelevant partitions
2. **Constraint Exclusion**: WHERE clauses determine which partitions to scan
3. **Inheritance**: Child partitions inherit indexes and constraints
4. **Detachment**: Old partitions become standalone tables (not deleted)
5. **Zero Downtime**: Partition operations don't lock the parent table
