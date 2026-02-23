use async_graphql::Schema;
use crate::AppState;
use crate::graphql::resolvers::{Query, Mutation, Subscription};

pub type AppSchema = Schema<Query, Mutation, Subscription>;

pub fn build_schema(state: AppState) -> AppSchema {
    Schema::build(
        Query::default(),
        Mutation::default(),
        Subscription::default(),
    )
    .data(state)
    .finish()
}
