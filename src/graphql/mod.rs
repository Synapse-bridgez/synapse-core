//! GraphQL API module with schema configuration, resolvers, and health checks.
//!
//! This module provides the GraphQL API endpoint for querying and mutating transactions
//! and settlements. It includes three categories of health checks:
//!
//! - **Schema Health**: The `build_schema()` function in [`schema`] verifies that the GraphQL
//!   schema can be constructed successfully with all extensions and resolvers initialized.
//!   If this fails, the entire GraphQL API is non-functional.
//!
//! - **Resolver Health**: Each resolver in [`resolvers`] performs implicit health checks
//!   by verifying database connectivity when executing queries (e.g., `queries::get_transaction`).
//!   If a resolver cannot reach the database, it returns an error and the query fails.
//!
//! - **Subscription Health**: WebSocket subscriptions in [`resolvers::TransactionSubscription`]
//!   require database connectivity to stream transaction updates. If the database becomes
//!   unavailable during a subscription, the connection is closed.
//!
//! The module also enforces query-level security checks:
//! - Query depth limit (max 10 levels) prevents stack overflow attacks
//! - Query complexity limit (max 1000 points) prevents expensive queries
//! - Alias limit (max 20 aliases) prevents bypassing other limits
//!
//! See [Health Checks Documentation](../../docs/graphql-health-checks.md) for detailed information.

pub mod error;
pub mod input_validation;
pub mod pagination;
pub mod rate_limiting;
pub mod resolvers;
pub mod schema;
pub mod shutdown;
pub mod validation;
