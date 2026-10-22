pub mod settlement;
pub mod swap;
pub mod transaction;

pub use settlement::{SettlementQuery, SettlementMutation};
pub use swap::{SwapMutation, SwapQuery};
pub use transaction::{TransactionMutation, TransactionQuery, TransactionSubscription};

use async_graphql::MergedObject;

#[derive(MergedObject, Default)]
pub struct Query(TransactionQuery, SettlementQuery, SwapQuery);

pub mod mutation {
    use super::settlement::SettlementMutation;
    use super::swap::SwapMutation;
    use super::transaction::TransactionMutation;
    use async_graphql::MergedObject;

    #[derive(MergedObject, Default)]
    pub struct Mutation(TransactionMutation, SettlementMutation, SwapMutation);
}

pub use mutation::Mutation;

pub mod subscription {
    use super::transaction::TransactionSubscription;
    use async_graphql::MergedSubscription;

    #[derive(MergedSubscription, Default)]
    pub struct Subscription(TransactionSubscription);
}

pub use subscription::Subscription;
