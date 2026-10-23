pub mod settlement;
pub mod transaction;

pub use settlement::SettlementQuery;
pub use transaction::{TransactionMutation, TransactionQuery, TransactionSubscription};

use async_graphql::MergedObject;

#[derive(MergedObject, Default)]
pub struct Query(TransactionQuery, SettlementQuery);

pub mod mutation {
    use super::transaction::TransactionMutation;
    use async_graphql::MergedObject;

    #[derive(MergedObject, Default)]
    pub struct Mutation(TransactionMutation);
}

pub use mutation::Mutation;

pub mod subscription {
    use super::transaction::TransactionSubscription;
    use async_graphql::MergedSubscription;

    #[derive(MergedSubscription, Default)]
    pub struct Subscription(TransactionSubscription);
}

pub use subscription::Subscription;
