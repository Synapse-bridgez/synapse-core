use crate::graphql::resolvers::{Mutation, Query, Subscription};
use crate::AppState;
use async_graphql::{
    extensions::{Extension, ExtensionContext, ExtensionFactory, NextParseQuery},
    parser::types::{ExecutableDocument, Selection},
    ServerError, ServerResult, Variables,
};
use std::sync::Arc;

pub type AppSchema = async_graphql::Schema<Query, Mutation, Subscription>;

/// Maximum query nesting depth allowed.
const MAX_QUERY_DEPTH: usize = 10;
/// Maximum query complexity score allowed.
const MAX_QUERY_COMPLEXITY: usize = 1000;
/// Maximum number of aliases allowed per query.
const MAX_QUERY_ALIASES: usize = 20;

/// Extension that rejects queries exceeding the alias limit.
struct AliasLimitExtension;

impl ExtensionFactory for AliasLimitExtension {
    fn create(&self) -> Arc<dyn Extension> {
        Arc::new(AliasLimitExtensionImpl)
    }
}

struct AliasLimitExtensionImpl;

/// Recursively count aliases in a selection set.
fn count_aliases(items: &[async_graphql::Positioned<Selection>]) -> usize {
    items
        .iter()
        .map(|sel| match &sel.node {
            Selection::Field(f) => {
                let self_alias = if f.node.alias.is_some() { 1 } else { 0 };
                self_alias
                    + count_aliases(&f.node.selection_set.node.items)
            }
            Selection::InlineFragment(frag) => {
                count_aliases(&frag.node.selection_set.node.items)
            }
            Selection::FragmentSpread(_) => 0,
        })
        .sum()
}

#[async_graphql::async_trait::async_trait]
impl Extension for AliasLimitExtensionImpl {
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
