//! GraphQL Schema Configuration
//!
//! This module configures the GraphQL schema with security extensions and query limits.
//! See [error_handling.md](./error_handling.md) for comprehensive error handling documentation.
//! See [../docs/graphql-health-checks.md](../docs/graphql-health-checks.md) for health check details.

use crate::graphql::rate_limiting::{GraphQlRateLimitConfig, GraphQlRateLimiter};
use crate::graphql::resolvers::{Mutation, Query, Subscription};
use crate::AppState;
use async_graphql::{
    extensions::{Extension, ExtensionContext, ExtensionFactory, NextParseQuery},
    parser::types::{ExecutableDocument, Selection},
    ServerError, ServerResult, Variables,
};
use std::sync::Arc;

/// The root GraphQL schema type for the application.
///
/// Combines Query, Mutation, and Subscription root types with applied
/// health checks and security extensions.
pub type AppSchema = async_graphql::Schema<Query, Mutation, Subscription>;

/// Maximum query nesting depth allowed.
///
/// Queries exceeding this depth are rejected to prevent stack overflow attacks.
/// A passing check means the query complexity is within safe limits for recursion.
const MAX_QUERY_DEPTH: usize = 10;

/// Maximum query complexity score allowed.
///
/// Query complexity is calculated as a weighted score of field selections.
/// Queries exceeding this limit are rejected to prevent expensive queries
/// that could cause denial of service. A passing check means the query
/// can execute efficiently without exhausting server resources.
const MAX_QUERY_COMPLEXITY: usize = 1000;

/// Maximum number of aliases allowed per query.
///
/// Query aliases are alternative names for fields. Excessive aliasing can be used
/// to bypass query limits. A passing check means the query does not attempt to
/// bypass security restrictions through aliasing. Exceeding this limit indicates
/// a potential attack attempt and the query is rejected.
const MAX_QUERY_ALIASES: usize = 20;

/// Extension that enforces the GraphQL alias limit security check.
///
/// Recursively counts all aliases in a query's selection set and rejects
/// the query if the count exceeds MAX_QUERY_ALIASES. This is a health check
/// that prevents queries from bypassing depth/complexity limits via aliasing.
///
/// **Health Check Details:**
/// - **What it verifies**: That queries do not contain excessive aliases that could bypass security limits
/// - **Passing result**: Query is allowed to proceed; alias count is within acceptable limits
/// - **Failing result**: Query is rejected with a ServerError describing the alias limit violation
/// - **Caller action on failure**: Retry with a query containing fewer aliases, or increase resource limits if legitimate
struct AliasLimitExtension;

impl ExtensionFactory for AliasLimitExtension {
    fn create(&self) -> Arc<dyn Extension> {
        Arc::new(AliasLimitExtensionImpl)
    }
}

/// Implementation of the alias limit extension health check.
///
/// Executes the alias count validation during the parse phase of every GraphQL query.
struct AliasLimitExtensionImpl;

/// Recursively counts all aliases in a GraphQL selection set.
///
/// Traverses the entire query tree (fields, inline fragments) to count how many
/// aliases are defined. This is used to enforce the alias limit security check.
///
/// # Arguments
///
/// * `items` - The selection set items to scan for aliases
///
/// # Returns
///
/// The total count of aliases found in the selection set and all nested selections.
fn count_aliases(items: &[async_graphql::Positioned<Selection>]) -> usize {
    items
        .iter()
        .map(|sel| match &sel.node {
            Selection::Field(f) => {
                let self_alias = if f.node.alias.is_some() { 1 } else { 0 };
                self_alias + count_aliases(&f.node.selection_set.node.items)
            }
            Selection::InlineFragment(frag) => count_aliases(&frag.node.selection_set.node.items),
            Selection::FragmentSpread(_) => 0,
        })
        .sum()
}

#[async_graphql::async_trait::async_trait]
impl Extension for AliasLimitExtensionImpl {
    /// Executes the alias limit health check on incoming GraphQL queries.
    ///
    /// This is a "resolver health" check that runs on every GraphQL request.
    /// It verifies that the query does not contain excessive aliases that could
    /// bypass depth or complexity limits.
    ///
    /// **Health Check Details:**
    /// - **What it verifies**: That the query's total alias count does not exceed MAX_QUERY_ALIASES
    /// - **Passing result**: The parsed document is returned unchanged and query execution proceeds
    /// - **Failing result**: A ServerError is returned indicating the alias count and limit; query is rejected
    /// - **Caller action on failure**: Remove aliases from the query or increase the global MAX_QUERY_ALIASES limit
    async fn parse_query(
        &self,
        ctx: &ExtensionContext<'_>,
        query: &str,
        variables: &Variables,
        next: NextParseQuery<'_>,
    ) -> ServerResult<ExecutableDocument> {
        let doc = next.run(ctx, query, variables).await?;

        let alias_count: usize = doc
            .operations
            .iter()
            .map(|(_, op)| count_aliases(&op.node.selection_set.node.items))
            .sum();

        if alias_count > MAX_QUERY_ALIASES {
            tracing::warn!(
                alias_count,
                max = MAX_QUERY_ALIASES,
                "GraphQL query rejected: too many aliases"
            );
            return Err(ServerError::new(
                format!(
                    "Query contains {} aliases, which exceeds the maximum of {}",
                    alias_count, MAX_QUERY_ALIASES
                ),
                None,
            ));
        }

        Ok(doc)
    }
}

/// Builds the GraphQL schema with all health checks and security extensions applied.
///
/// This function performs the "schema health" check: verifying that the schema
/// can be constructed successfully with all resolvers and extensions initialized.
///
/// **Health Check Details:**
/// - **What it verifies**: That the GraphQL schema initializes successfully with all
///   extensions loaded and that the application state can be injected into resolvers.
///   If this fails, the GraphQL layer is broken and queries cannot execute.
/// - **Passing result**: A fully constructed AppSchema with depth limits, complexity limits,
///   alias limits, and all resolvers ready to handle queries
/// - **Failing result**: Schema construction panics or fails, indicating misconfiguration
///   of resolvers or state
/// - **Caller action on failure**: Fix the underlying resolver or state issue; the entire
///   GraphQL API is non-functional until resolved
///
/// # Arguments
///
/// * `state` - The application state (database, auth, services, etc.) made available
///   to all resolvers via async_graphql::Context
///
/// # Panics
///
/// Panics if the schema cannot be built (e.g., resolver conflict, invalid field names).
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
    .extension(GraphQlRateLimiter::new(GraphQlRateLimitConfig::default()))
    .finish()
}
