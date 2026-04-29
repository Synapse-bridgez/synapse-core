# ADR-002: Circuit Breaker Pattern for External APIs

## Status

Accepted

## Context

Synapse Core integrates with external services, primarily the **Stellar Horizon API**, for:

- Account verification
- Transaction validation
- On-chain data queries
- Asset information lookups

External API failures can cause cascading problems:

- **Thread exhaustion** - Worker threads pile up waiting for timeouts
- **Resource waste** - Repeated calls to a known-down service
- **Slow failure detection** - Each request waits for full timeout (30+ seconds)
- **Poor user experience** - Long response times during outages
- **Cascading failures** - Downstream services affected by our slowness

Without a circuit breaker, when Horizon API experiences issues:
1. Requests timeout after 30 seconds
2. Thread pool fills with waiting requests
3. New requests queue up
4. Application becomes unresponsive
5. Potential OOM or crash

## Decision

We will implement the **Circuit Breaker pattern** for all external API clients, starting with the Stellar Horizon client.

**Implementation using the `failsafe` crate:**

```rust
pub struct HorizonClient {
    base_url: String,
    client: reqwest::Client,
    circuit_breaker: Arc<Mutex<CircuitBreaker>>,
}
```

**Circuit breaker states:**

1. **Closed (Normal)** - Requests pass through to API
2. **Open (Fail Fast)** - Requests immediately rejected without calling API
3. **Half-Open (Probing)** - After timeout, allow test requests to check recovery

**Configuration:**

- **Failure threshold:** 5 consecutive failures
- **Reset timeout:** 60 seconds (with jitter)
- **Failure policy:** Consecutive failures (not percentage-based)
- **Backoff strategy:** Equal jittered (prevents thundering herd)

**Error handling:**

```rust
match client.get_account(address).await {
    Err(HorizonError::CircuitBreakerOpen) => {
        // Return 503 Service Unavailable
        // Log incident
        // Trigger alert
    }
    // ... other error handling
}
```

## Consequences

### Positive

- **Fast failure** - Immediate rejection when service is known to be down (milliseconds vs 30+ seconds)
- **Resource protection** - Prevents thread pool exhaustion
- **Automatic recovery** - Probes for service recovery without manual intervention
- **Better observability** - Clear signal when external service is down
- **Improved user experience** - Fast error responses instead of timeouts
- **Prevents cascading failures** - Isolates external service issues
- **Configurable** - Adjust thresholds per service requirements

### Negative

- **False positives** - Transient errors may open circuit unnecessarily
- **Delayed recovery** - Service may recover before circuit closes
- **Additional complexity** - More code to maintain and test
- **Monitoring required** - Need to track circuit breaker state
- **Tuning needed** - Thresholds may need adjustment based on real-world behavior

### Neutral

- **Transparent to callers** - Error handling remains similar
- **Thread-safe** - Can be cloned and shared across tasks
- **Minimal overhead** - Negligible performance impact when closed

## Alternatives Considered

### Alternative 1: Retry with Exponential Backoff

**Description:** Retry failed requests with increasing delays (2s, 4s, 8s, etc.)

**Pros:**
- Simpler to implement
- Handles transient errors well
- No state management

**Cons:**
- Still wastes resources on known-down services
- Slow to detect persistent failures
- Each request retries independently (no shared state)
- Can amplify load on recovering service

**Why rejected:** Doesn't prevent resource exhaustion during prolonged outages. Retries are complementary to circuit breakers, not a replacement.

### Alternative 2: Health Check Endpoint

**Description:** Periodically poll Horizon's health endpoint; disable client if unhealthy.

**Pros:**
- Proactive failure detection
- Centralized health monitoring
- Can check multiple endpoints

**Cons:**
- Requires dedicated health check endpoint (not all APIs have one)
- Polling overhead
- Delay between health check and detection
- Doesn't handle partial failures (some endpoints down, others up)

**Why rejected:** Reactive approach (circuit breaker) is more responsive to actual failures. Health checks are complementary for monitoring but don't replace circuit breakers.

### Alternative 3: Timeout Reduction

**Description:** Reduce request timeout from 30s to 5s to fail faster.

**Pros:**
- Simple configuration change
- Faster failure detection
- No additional code

**Cons:**
- May cause false positives on slow networks
- Still wastes resources waiting for timeout
- Doesn't prevent repeated calls to down service
- Doesn't provide automatic recovery

**Why rejected:** Addresses symptom (slow failures) but not root cause (repeated calls to down service). Circuit breaker provides better solution.

### Alternative 4: Manual Circuit Breaker

**Description:** Implement custom circuit breaker logic without external library.

**Pros:**
- Full control over behavior
- No external dependencies
- Can customize for specific needs

**Cons:**
- Significant development effort
- Potential bugs in implementation
- Need to handle thread safety
- Reinventing the wheel

**Why rejected:** `failsafe` crate is well-tested, maintained, and provides exactly what we need. No reason to reimplement.

### Alternative 5: Service Mesh (Istio, Linkerd)

**Description:** Use service mesh for circuit breaking, retries, and timeouts.

**Pros:**
- Centralized traffic management
- Language-agnostic
- Advanced features (traffic splitting, observability)
- No application code changes

**Cons:**
- Significant infrastructure complexity
- Requires Kubernetes
- Overkill for current deployment
- Learning curve for team
- Additional operational overhead

**Why rejected:** Premature for current scale. Service mesh is valuable at larger scale with many services, but adds unnecessary complexity now.

## Implementation Notes

### Integration with Horizon Client

```rust
impl HorizonClient {
    pub fn new(base_url: String) -> Self {
        Self::with_circuit_breaker_config(
            base_url,
            5,                              // failure_threshold
            Duration::from_secs(60),        // reset_timeout
        )
    }
    
    pub async fn get_account(&self, address: &str) -> Result<Account, HorizonError> {
        // Circuit breaker wraps the actual API call
        self.circuit_breaker
            .call(|| self.fetch_account(address))
            .await
            .map_err(|e| match e {
                FailureError::CircuitBreakerOpen => HorizonError::CircuitBreakerOpen,
                FailureError::Inner(e) => e,
            })
    }
}
```

### Error Response Mapping

When circuit breaker is open:

```rust
// In Axum handler
match horizon_client.get_account(address).await {
    Err(HorizonError::CircuitBreakerOpen) => {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "Stellar Horizon API is temporarily unavailable",
                "retry_after": 60
            }))
        )
    }
    // ... other error handling
}
```

### Monitoring and Alerting

Expose circuit breaker state:

```rust
// Health check endpoint
pub async fn health_check(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy",
        horizon_circuit: state.horizon_client.circuit_state(), // "open" or "closed"
    })
}
```

**Metrics to track:**
- Circuit breaker state changes (closed → open, open → half-open, half-open → closed)
- Time spent in open state
- Number of rejected requests while open
- Failure rate before opening

**Alerts:**
- Circuit breaker opened (immediate)
- Circuit breaker open for >5 minutes (escalate)
- Frequent state changes (flapping)

### Testing

```rust
#[tokio::test]
async fn test_circuit_breaker_opens_after_failures() {
    let mut server = mockito::Server::new_async().await;
    
    // Mock 5 consecutive failures
    let mock = server.mock("GET", "/accounts/test")
        .with_status(500)
        .expect(5)
        .create_async()
        .await;
    
    let client = HorizonClient::new(server.url());
    
    // First 5 requests fail and count toward threshold
    for _ in 0..5 {
        let result = client.get_account("test").await;
        assert!(result.is_err());
    }
    
    // 6th request should be rejected by circuit breaker
    let result = client.get_account("test").await;
    assert!(matches!(result, Err(HorizonError::CircuitBreakerOpen)));
    
    mock.assert_async().await;
}
```

### Configuration

Environment variables for tuning:

```env
# Optional: Override default circuit breaker settings
HORIZON_CIRCUIT_BREAKER_THRESHOLD=5
HORIZON_CIRCUIT_BREAKER_TIMEOUT_SECS=60
```

### Future Enhancements

1. **Per-endpoint circuit breakers** - Separate circuits for different API endpoints
2. **Adaptive thresholds** - Adjust based on historical failure rates
3. **Metrics export** - Prometheus metrics for circuit breaker state
4. **Dashboard** - Visualize circuit breaker state and history
5. **Graceful degradation** - Fallback to cached data when circuit is open

## References

- [Circuit Breaker Pattern (Martin Fowler)](https://martinfowler.com/bliki/CircuitBreaker.html)
- [Release It! (Michael Nygard)](https://pragprog.com/titles/mnee2/release-it-second-edition/)
- [failsafe crate documentation](https://docs.rs/failsafe/)
- [docs/circuit-breaker.md](../circuit-breaker.md) - Implementation guide
- Issue #18 - Circuit breaker implementation
