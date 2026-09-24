# ADR-008: Multi-Region Circuit Breaker State Coordination

## Status

Proposed

## Context

As described in ADR-002, Synapse Core implements the Circuit Breaker pattern for external API resilience. That decision assumes a single-region deployment where the in-process circuit breaker state is shared implicitly across all instances via immediate memory access.

In a future multi-region deployment, each region would run independent instances of Synapse Core. Without cross-region coordination of circuit breaker state, this creates a consistency problem:

- **Region A**: Correctly detects that Horizon is down and opens its circuit breaker.
- **Region B**: May remain unaware, continuing to hammer an actually-down endpoint while Region A has already correctly failed-fast.
- **Conversely**: Region B may prematurely open based on transient latency, causing Region A to unnecessarily degrade service quality.

The circuit breaker is also in-process (per ADR-002), meaning there is no natural synchronization mechanism. The current Redis-based state persistence (in `src/services/circuit_breaker.rs`) stores state in a single Redis instance; a true multi-region design must account for Redis placement and network partition scenarios.

## Problem Statement & Context

**Current Deployment Model**: Single-region, multiple instances behind a load balancer, one Redis instance.  
**Future Multi-Region Target**: Multiple independent regional deployments, each with its own instances and potentially its own Redis.

**Key tensions**:

1. **Coordination overhead vs. blast-radius reduction**
   - Fully independent per-region breakers: Minimal overhead, but each region may continue hammering a down endpoint.
   - Shared external state store: Adds latency and failure dependencies, but gives all regions a unified view.
   - Gossip-based: Hybrid approach with message-passing overhead and eventual-consistency delays.

2. **Network partition behavior**
   - If the Redis store is shared across regions and the inter-region network fails, one region's circuit state becomes inaccessible to others.
   - If each region has its own Redis, a transient failure in one region doesn't affect others, but coordination is lost.

3. **Recovery signaling**
   - When a circuit breaker transitions to Half-Open (probing for recovery), all regions should ideally probe concurrently rather than sequentially, to avoid redundant probes and reduce mean-time-to-recovery.

## Decision

We recommend a **Shared External State Store** approach with per-region resilience as the long-term multi-region coordination strategy, with an explicit fallback to per-region independence during network partitions.

### Rationale

1. **Unified view**: All regions observe the same circuit state, reducing the risk of one region's transient error affecting others.
2. **Recovery coordination**: When a circuit transitions to Half-Open, all regions see this state and coordinate their probes.
3. **Graceful degradation**: Each region can fall back to per-region state during a partition, resuming coordination when connectivity restores.
4. **Operational visibility**: A single source of truth for circuit state simplifies monitoring and debugging.

### Design Principles

1. **Per-region resilience**: Each regional deployment has a read-only replica of circuit state, refreshed periodically. A replica failure or network partition doesn't cascade.
2. **Write-through with fallback**: Circuit state changes are written to a central store (e.g., shared PostgreSQL, a dedicated coordination service). On write failure, the region temporarily reverts to per-region state and continues operation.
3. **Eventual consistency with bounded skew**: State changes propagate across regions within seconds, bounded by polling intervals.
4. **Explicit partition handling**: Operators are alerted when a region is partitioned from the central state store, and can manually choose to remain independent or fail-over.

### Implementation Strategy (Prototype Phase)

For this issue, we prototype the shared-store approach with an in-memory cache fallback:

1. **Central coordination store**: Use PostgreSQL (already part of the stack) as the source of truth for circuit breaker state.
2. **Per-region cache**: Each region polls the central store every 5–10 seconds, caching state locally.
3. **Write-through**: State changes are persisted to PostgreSQL first; local in-process state is updated only on successful write.
4. **Partition detection**: If PostgreSQL writes fail for >N seconds, the region logs a warning and falls back to per-region-only operation (continuing to use local state without central sync).
5. **Recovery**: When connectivity restores, the region re-syncs with the central store.

### Database Schema

Add a new table to track circuit breaker state across regions:

```sql
CREATE TABLE IF NOT EXISTS circuit_breaker_state (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    service_name TEXT NOT NULL,
    region_name TEXT NOT NULL,  -- e.g., "us-east-1"
    state TEXT NOT NULL,        -- 'closed', 'open', 'half_open'
    failure_count INTEGER NOT NULL DEFAULT 0,
    opened_at TIMESTAMPTZ,
    last_error TEXT,
    last_sync_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(service_name, region_name)
);

CREATE INDEX idx_circuit_breaker_service ON circuit_breaker_state(service_name);
```

## Alternatives Considered

### Alternative 1: Fully Independent Per-Region Breakers

**Description**: Each region maintains its own circuit state, with no coordination.

**Pros**:
- Simplest to implement and operate.
- No inter-region dependency; a regional failure is isolated.

**Cons**:
- Multiple regions may all hammer a down endpoint independently, wasting resources.
- No way to share recovery information; if Region A detects recovery, Region B still doesn't know.
- Operational visibility is poor; operators must check each region's breaker state separately.

**Why not chosen**: Fails to solve the core problem of coordinating awareness of external service failures across regions.

### Alternative 2: Gossip-Based State Propagation

**Description**: Each region broadcasts its circuit state periodically to other regions via a publish-subscribe mechanism (e.g., Redis Pub/Sub, Kafka).

**Pros**:
- Decentralized; no single point of coordination.
- Scales well as regions are added.
- Eventual consistency is natural.

**Cons**:
- Eventual consistency means regions may diverge for seconds or longer.
- Requires a separate pub-sub infrastructure (adds complexity).
- Determining which region's state "wins" on conflict requires a tie-breaking policy.
- Harder to debug and audit.

**Why not chosen**: Shared state store is simpler to reason about and debug. Gossip can be considered if regional latency requirements tighten in the future.

### Alternative 3: Service Mesh-Based Circuit Breaking

**Description**: Use Istio or Linkerd to manage circuit breaking at the sidecar level, decoupled from the application.

**Pros**:
- Centralized traffic management.
- Language-agnostic; benefits all services in the mesh.
- Built-in observability.

**Cons**:
- Requires Kubernetes and a service mesh (significant operational overhead).
- Each sidecar must still coordinate with others; the problem isn't solved by delegating to the mesh.
- Overkill for the current scale and architecture.

**Why not chosen**: Not yet justified by deployment scale or operational readiness.

## Prototype Scope

This issue produces:

1. **Updated ADR** (this document) with the recommendation and design.
2. **Migration**: SQL schema for the central circuit breaker state table.
3. **Coordinator module**: New `src/services/circuit_breaker_coordinator.rs` that:
   - Polls PostgreSQL for current state (cache with TTL).
   - Implements write-through on state changes.
   - Handles network partition (falls back to per-region state, logs warnings).
   - Exports metrics on sync health.
4. **Tests**: 
   - Happy-path test: Multi-region state coordination under normal conditions.
   - Partition test: Simulate a region losing connectivity; verify it falls back to per-region state and resumes syncing when partition heals.
   - Concurrent-update test: Multiple regions updating state concurrently; verify no lost updates.
5. **Documentation**: `docs/multi-region-deployment.md` explains the deployment model and operational runbook (e.g., how to handle a partitioned region).

### Out of Scope

- Full production rollout of multi-region deployment (this issue is design + prototype).
- Optimization of cache invalidation (TTL-based polling is acceptable for prototype).
- Support for >3 regions (scale testing).
- Integration with external coordination services (Consul, etcd).

## Consequences

### Positive

- **Unified failure awareness**: All regions see the same circuit breaker state, reducing redundant API calls.
- **Coordinated recovery**: Regions can probe for recovery in parallel, improving mean-time-to-recovery.
- **Operational simplicity**: Single source of truth for circuit state.
- **Graceful degradation**: Per-region fallback ensures service continuity during coordination outages.

### Negative

- **Added complexity**: Coordinator module and database state increase codebase complexity.
- **Latency on state changes**: Changes must be persisted to PostgreSQL before taking effect; adds latency (typically <100ms).
- **Operational dependency**: Service health now depends on PostgreSQL connectivity. A PostgreSQL outage does not break the service (per-region fallback), but coordination is lost.

### Neutral

- **Observability**: Requires new metrics (sync health, partition detection) but provides better insight into multi-region state.

## Implementation Notes

### Coordinator Logic

```rust
pub struct CircuitBreakerCoordinator {
    pool: PgPool,
    service_name: String,
    region_name: String,
    cache: Arc<Mutex<CircuitBreakerState>>,
    cache_ttl: Duration,
    last_sync: Arc<Mutex<Instant>>,
    sync_failure_threshold: Duration, // Time before falling back to per-region
}

impl CircuitBreakerCoordinator {
    /// Fetch state from central store, or return cached state if stale.
    pub async fn get_state(&self) -> Result<CircuitBreakerState, CoordinatorError> {
        // Try to fetch from central store if cache is stale
        if self.last_sync.elapsed() > self.cache_ttl {
            match self.sync_from_central().await {
                Ok(state) => {
                    *self.cache.lock().await = state;
                    return Ok(state);
                }
                Err(e) if self.last_sync.elapsed() > self.sync_failure_threshold => {
                    // Partition detected; log warning and fall back to cache
                    tracing::warn!("Circuit breaker sync failed; falling back to local state");
                    return Ok(self.cache.lock().await.clone());
                }
                Err(e) => return Err(e),
            }
        }
        Ok(self.cache.lock().await.clone())
    }

    /// Persist a state change to the central store.
    pub async fn set_state(&self, state: CircuitBreakerState) -> Result<(), CoordinatorError> {
        // Write-through: persist to central store first
        self.persist_to_central(&state).await?;
        // On success, update local cache
        *self.cache.lock().await = state;
        Ok(())
    }

    async fn sync_from_central(&self) -> Result<CircuitBreakerState, CoordinatorError> {
        // Query PostgreSQL for the current state
        // Return error if unreachable
    }

    async fn persist_to_central(&self, state: &CircuitBreakerState) -> Result<(), CoordinatorError> {
        // Upsert state into PostgreSQL
        // Return error if unreachable
    }
}
```

### Testing Under Partition

```rust
#[tokio::test]
async fn test_partition_falls_back_to_per_region() {
    // 1. Set up coordinator with a working DB connection
    let coord = CircuitBreakerCoordinator::new(pool, sync_failure_threshold: 5s);
    
    // 2. Verify state is synced from central
    let state = coord.get_state().await.unwrap();
    assert!(state.synced);
    
    // 3. Simulate network partition by stopping DB connection
    // (or mock the DB to return errors)
    
    // 4. Wait for sync to fail >5s; verify fallback
    tokio::time::sleep(Duration::from_secs(6)).await;
    let state = coord.get_state().await.unwrap();
    assert!(!state.synced); // Fell back to local
    
    // 5. Restore DB connection; verify re-sync
    // (resume DB mocks to return success)
    let state = coord.get_state().await.unwrap();
    assert!(state.synced); // Re-synced
}
```

## Testing Strategy

1. **Unit tests**: 
   - Coordinator cache hit/miss.
   - Write-through success and fallback.
   - Partition detection and recovery.

2. **Integration tests**: 
   - Multi-region state sync with a real PostgreSQL instance.
   - Concurrent state updates from multiple regions.
   - Partition healing (simulated via DB connection toggle).

3. **Operational tests**: (manual or long-running)
   - Steady-state syncing under normal conditions.
   - Partition recovery behavior.
   - Metrics correctness.

## Monitoring & Alerts

**Metrics**:
- `circuit_breaker_sync_latency_ms`: Time to fetch state from central store.
- `circuit_breaker_sync_failures_total`: Total sync failures.
- `circuit_breaker_partitioned_regions`: Number of regions partitioned from central store.

**Alerts**:
- Region is partitioned from central store for >5 minutes.
- Central store is unreachable by all regions.

## Migration Path

1. **Phase 1 (this issue)**: Prototype and validate design.
2. **Phase 2**: Deploy coordinator in read-only mode to validate metrics and observe behavior.
3. **Phase 3**: Enable write-through in a non-critical region, monitor for issues.
4. **Phase 4**: Gradual rollout to all regions with active fallback and partition detection.

## References

- ADR-002: Circuit Breaker Pattern
- `src/services/circuit_breaker.rs`: Current in-process implementation
- `src/db/models.rs`: Database models
- Designing Data-Intensive Applications (Martin Kleppmann), Chapter 8: Distributed System Challenges

## Decision Rationale Summary

The shared external state store approach with per-region resilience provides the best balance between:
- **Coordination benefits** (unified awareness, coordinated recovery) vs. overhead
- **Operational simplicity** vs. distributed-system complexity
- **Fault isolation** (per-region fallback) vs. tight coupling

This design is appropriate for a future multi-region deployment of Synapse Core and can be extended or replaced with gossip-based or service-mesh approaches if operational requirements change.
