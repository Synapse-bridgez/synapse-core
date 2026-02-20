pub mod transaction;
pub mod settlement;

pub use transaction::{TransactionQuery, TransactionMutation, TransactionSubscription};
pub use settlement::SettlementQuery;

use async_graphql::MergedObject;

#[derive(MergedObject, Default)]
pub struct Query(TransactionQuery, SettlementQuery);

pub mod mutation {
    use async_graphql::MergedObject;
    use super::transaction::TransactionMutation;

    #[derive(MergedObject, Default)]
    pub struct Mutation(TransactionMutation);
}

pub use mutation::Mutation;

pub mod subscription {
    use async_graphql::MergedSubscription;
    use super::transaction::TransactionSubscription;

    #[derive(MergedSubscription, Default)]
    pub struct Subscription(TransactionSubscription);
}

pub use subscription::Subscription;
