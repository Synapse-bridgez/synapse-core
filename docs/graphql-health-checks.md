# GraphQL Health Checks

This document describes the health checks implemented in the GraphQL module and their integration with the broader `/health` endpoint.

## Overview

The GraphQL module implements three categories of health checks:

1. **Schema Health** — Verifies the GraphQL schema initializes successfully
2. **Resolver Health** — Checks database connectivity during query execution
3. **Subscription Health** — Ensures WebSocket subscriptions can stream data from the database
4. **Query Validation Health** — Enforces security limits on incoming queries

## Schema Health Check

### Description

The schema health check verifies that the GraphQL schema can be constructed successfully with all extensions and resolvers initialized. This check runs once during application startup.

### Implementation

Located in `src/graphql/schema.rs::build_schema()`:

```rust
pub fn build_schema(state: AppState) -> AppSchema {
    async_graphql::Schema::build(
        Query::default(),
        Mutation::default(),
        Subscription::default(),
    )
    .data(state)
    .limit_depth(MAX_QUERY_DEPTH)
    .limit_complexity(MAX_QUERY_COMPLEXITY)
    .limit_recursive_depth(MAX_QUERY_DEPTH)
    .extension(AliasLimitExtension)
    .finish()
}
```

### What a Passing Result Means

- The GraphQL schema successfully initializes with all resolvers and extensions loaded
- The application state (database, auth, services) can be injected into resolvers
- All GraphQL operations can theoretically execute (further checks may still fail)

### What a Failing Result Means

- The schema cannot be constructed (resolver conflict, invalid field names, missing dependencies)
- The entire GraphQL API is non-functional
- The application may fail to start

### What Callers Should Do on Failure

Fix the underlying resolver or state initialization issue. The GraphQL API cannot serve requests until the schema builds successfully.

## Resolver Health Check

### Description

Resolvers perform implicit health checks by verifying database connectivity when executing queries. Every resolver that queries the database indirectly checks if the database is reachable and responsive.

### Implementation

Located in `src/graphql/resolvers/transaction.rs` and `src/graphql/resolvers/settlement.rs`:

Each resolver calls database query functions like:
- `queries::get_transaction()`
- `queries::list_transactions()`
- `queries::get_settlement()`
- etc.

These functions return errors if the database cannot be reached.

### What a Passing Result Means

- The database is reachable and responsive
- The query was executed successfully
- Transaction/settlement data is available

### What a Failing Result Means

- The database connection is unavailable
- A timeout occurred while waiting for the database response
- Database credentials or permissions are invalid

### What Callers Should Do on Failure

Check the database status, verify connectivity, and retry the query. If failures persist, investigate database logs and network connectivity.

## Subscription Health Check

### Description

WebSocket subscriptions in `TransactionSubscription` require database connectivity to stream updates. If the database becomes unavailable during a subscription, the connection is closed.

### Implementation

Located in `src/graphql/resolvers/transaction.rs::Subscription::on_transaction_update()`:

The subscription maintains an open connection and streams transaction updates from the database. If any database operation fails, the subscription terminates.

### What a Passing Result Means

- The WebSocket connection is established
- The database is reachable
- Updates are successfully streamed to the client

### What a Failing Result Means

- The database connection failed during streaming
- Network connectivity was lost
- The subscription was cancelled

### What Callers Should Do on Failure

Reconnect to the subscription endpoint and re-establish the WebSocket connection. Investigate database and network status.

## Query Validation Health Checks

### Description

Three security checks run on every GraphQL query to prevent malicious or expensive queries:

1. **Depth Limit Check** (MAX_QUERY_DEPTH = 10)
2. **Complexity Limit Check** (MAX_QUERY_COMPLEXITY = 1000)
3. **Alias Limit Check** (MAX_QUERY_ALIASES = 20)

### Implementation

Located in `src/graphql/schema.rs`:

- **Depth/Complexity**: Configured via `.limit_depth()` and `.limit_complexity()` when building the schema
- **Alias Limit**: Implemented via the `AliasLimitExtension` which counts aliases during query parsing

### What Passing Results Mean

- The query is within acceptable complexity and depth bounds
- The query does not contain excessive aliases
- The query can execute safely without exhausting server resources

### What Failing Results Mean

- The query exceeds depth, complexity, or alias limits
- The query may be malicious or poorly constructed
- The query is rejected before execution

### What Callers Should Do on Failure

Simplify the query:
- Remove nested selections or use fragments to reduce depth
- Use field selections to reduce complexity
- Avoid using aliases; use GraphQL fragments instead
- Contact API support if legitimate use cases are being blocked

## Integration with `/health` Endpoint

The GraphQL health status is integrated into the broader `/health` endpoint as follows:

- **Schema Health**: Checked during application startup; if it fails, the entire application fails to start
- **Resolver Health**: Implicitly checked whenever a query is executed; failures are reported as query errors
- **Query Validation Health**: Results are reported in query responses; failed checks return GraphQL errors

The `/health` endpoint itself does not directly probe GraphQL resolvers, but monitors overall database connectivity which indirectly reflects GraphQL resolver health.

## Expected Behavior When Async-GraphQL Schema Fails to Initialize

If the async-graphql schema fails to initialize:

1. **Application Startup Failure**: The application will fail to start during the initialization phase
2. **Error Messages**: Will contain details about the schema construction failure (e.g., resolver conflicts)
3. **Mitigation**: Fix the resolver or state issue and restart the application

Examples of schema initialization failures:
- Resolver field name conflicts
- Invalid async-graphql attributes
- Missing or null application state
- Conflicting type definitions

## How to Add a New GraphQL Health Check

To add a new health check to the GraphQL module:

1. **Schema Health**: Modify `build_schema()` to add schema configuration or extensions
   - Example: Add a new `.extension(MyHealthCheckExtension)` before `.finish()`
   
2. **Resolver Health**: Add database queries or service calls to resolvers
   - Example: Add `services::check_cache_health()` to verify cache connectivity
   
3. **Query Validation**: Create a new extension or modify `AliasLimitExtension`
   - Example: Add a new extension struct and implement `Extension` trait
   - Override `parse_query()` to intercept and validate queries
   
4. **Documentation**: Update this file with the new check description and implementation details

### Example: Adding a Custom Query Validation Extension

```rust
// In src/graphql/schema.rs

struct CustomValidationExtension;

impl ExtensionFactory for CustomValidationExtension {
    fn create(&self) -> Arc<dyn Extension> {
        Arc::new(CustomValidationExtensionImpl)
    }
}

struct CustomValidationExtensionImpl;

#[async_graphql::async_trait::async_trait]
impl Extension for CustomValidationExtensionImpl {
    async fn parse_query(
        &self,
        ctx: &ExtensionContext<'_>,
        query: &str,
        variables: &Variables,
        next: NextParseQuery<'_>,
    ) -> ServerResult<ExecutableDocument> {
        // Validate query
        if !is_valid_query(query) {
            return Err(ServerError::new("Query validation failed", None));
        }
        next.run(ctx, query, variables).await
    }
}

// Add to build_schema():
.extension(CustomValidationExtension)
```

## Monitoring and Alerting

Monitor GraphQL health through:

- **Application startup logs**: Look for schema initialization errors
- **Query error rates**: Track percentage of queries failing
- **Database connection pool metrics**: Monitor resolver health indirectly
- **WebSocket subscription errors**: Track subscription termination rates

Set up alerts for:
- Schema initialization failures (blocking)
- Sustained high query error rates (>5% errors)
- Database unavailability (blocks all resolvers)
