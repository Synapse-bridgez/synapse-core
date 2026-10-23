//! Typed GraphQL query/response types generated from the live Synapse schema
//! (issue #1351).
//!
//! # Purpose
//!
//! The server exposes a `POST /graphql` endpoint backed by the schema defined
//! in `src/graphql/schema.rs` and its resolvers.  This module provides
//! strongly-typed Rust structs and builders for every operation that schema
//! supports, eliminating the stringly-typed `serde_json::Value` round-trip
//! that callers would otherwise need.
//!
//! Types are hand-generated from the live schema rather than produced by a
//! build-time macro/proc-macro, which keeps the SDK dependency surface small
//! (no `graphql_client` or `cynic` dev dependency).  The schema is reproduced
//! in inline SDL comments so reviewers can verify correctness without running
//! the server.
//!
//! # Schema (SDL snapshot — `src/graphql/schema.rs`)
//!
//! ```graphql
//! type Transaction {
//!     id: ID!
//!     stellar_account: String!
//!     amount: String!
//!     asset_code: String!
//!     status: String!
//!     created_at: String!
//!     updated_at: String!
//!     anchor_transaction_id: String
//!     memo: String
//!     memo_type: String
//! }
//!
//! type Settlement {
//!     id: ID!
//!     asset_code: String!
//!     total_amount: String!
//!     tx_count: Int!
//!     period_start: String!
//!     period_end: String!
//!     status: String!
//!     created_at: String!
//!     updated_at: String!
//! }
//!
//! input TransactionFilter {
//!     status: String
//!     asset_code: String
//!     stellar_account: String
//! }
//!
//! type Query {
//!     transaction(id: ID!): Transaction!
//!     transactions(filter: TransactionFilter, limit: Int, offset: Int): [Transaction!]!
//!     settlement(id: ID!): Settlement!
//!     settlements(limit: Int, offset: Int): [Settlement!]!
//! }
//!
//! type Mutation {
//!     forceCompleteTransaction(id: ID!): Transaction!
//!     replayDlq(id: ID!): Boolean!
//! }
//!
//! type Subscription {
//!     transactionStatusChanged(
//!         transactionId: ID,
//!         assetCode: String
//!     ): TransactionStatusUpdate!
//! }
//! ```
//!
//! # Usage
//!
//! ```no_run
//! use synapse_sdk::graphql_typed::{
//!     GetTransactionQuery, GetTransactionVariables,
//!     ListTransactionsQuery, ListTransactionsVariables, TransactionFilter,
//! };
//! use synapse_sdk::SynapseClient;
//!
//! # #[tokio::main]
//! # async fn main() {
//! let client = SynapseClient::new("https://api.example.com", "key");
//!
//! // Fetch a single transaction by ID.
//! let vars = GetTransactionVariables { id: "550e8400-e29b-41d4-a716-446655440000".into() };
//! let resp = client.graphql().execute::<GetTransactionQuery>(vars).await.unwrap();
//! println!("{:?}", resp.data);
//!
//! // List transactions with an optional filter.
//! let vars = ListTransactionsVariables {
//!     filter: Some(TransactionFilter {
//!         status: Some("pending".into()),
//!         asset_code: Some("USD".into()),
//!         stellar_account: None,
//!     }),
//!     limit: Some(20),
//!     offset: None,
//! };
//! let resp = client.graphql().execute::<ListTransactionsQuery>(vars).await.unwrap();
//! for tx in resp.data.unwrap_or_default() {
//!     println!("{} {}", tx.id, tx.status);
//! }
//! # }
//! ```

use serde::{Deserialize, Serialize};

// ── Trait ─────────────────────────────────────────────────────────────────────

/// A typed GraphQL operation (query or mutation).
///
/// Implement this trait for any query/mutation struct to enable use with
/// [`crate::resources::graphql::GraphQL::execute`].
pub trait GraphQLOperation {
    /// The variables struct serialized into the `"variables"` key.
    type Variables: Serialize;
    /// The `"data"` payload deserialized from a successful response.
    type ResponseData: for<'de> Deserialize<'de>;

    /// The GraphQL document string (query or mutation).
    fn document() -> &'static str;
}

// ── Shared field types ────────────────────────────────────────────────────────

/// A Transaction as returned by GraphQL queries.
///
/// Field names mirror the SDL definition in `src/graphql/resolvers/transaction.rs`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GqlTransaction {
    pub id: String,
    pub stellar_account: String,
    pub amount: String,
    pub asset_code: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub anchor_transaction_id: Option<String>,
    pub memo: Option<String>,
    pub memo_type: Option<String>,
}

/// A Settlement as returned by GraphQL queries.
///
/// Field names mirror the SDL definition in `src/graphql/resolvers/settlement.rs`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GqlSettlement {
    pub id: String,
    pub asset_code: String,
    pub total_amount: String,
    pub tx_count: i32,
    pub period_start: String,
    pub period_end: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub dispute_reason: Option<String>,
}

/// Input object for filtering transactions.
///
/// Mirrors the `TransactionFilter` input type in the schema.
#[derive(Debug, Default, Clone, Serialize)]
pub struct TransactionFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stellar_account: Option<String>,
}

// ── query { transaction(id: ID!): Transaction! } ──────────────────────────────

/// Typed variables for `GetTransactionQuery`.
#[derive(Debug, Clone, Serialize)]
pub struct GetTransactionVariables {
    pub id: String,
}

/// Response data for `GetTransactionQuery`.
#[derive(Debug, Clone, Deserialize)]
pub struct GetTransactionData {
    pub transaction: GqlTransaction,
}

/// Typed GraphQL operation: fetch a single transaction by ID.
pub struct GetTransactionQuery;

impl GraphQLOperation for GetTransactionQuery {
    type Variables = GetTransactionVariables;
    type ResponseData = GetTransactionData;

    fn document() -> &'static str {
        r#"
        query GetTransaction($id: ID!) {
            transaction(id: $id) {
                id
                stellar_account
                amount
                asset_code
                status
                created_at
                updated_at
                anchor_transaction_id
                memo
                memo_type
            }
        }
        "#
    }
}

// ── query { transactions(...): [Transaction!]! } ──────────────────────────────

/// Typed variables for `ListTransactionsQuery`.
#[derive(Debug, Default, Clone, Serialize)]
pub struct ListTransactionsVariables {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<TransactionFilter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i32>,
}

/// Response data for `ListTransactionsQuery`.
#[derive(Debug, Clone, Deserialize)]
pub struct ListTransactionsData {
    pub transactions: Vec<GqlTransaction>,
}

/// Typed GraphQL operation: list transactions with optional filter.
pub struct ListTransactionsQuery;

impl GraphQLOperation for ListTransactionsQuery {
    type Variables = ListTransactionsVariables;
    type ResponseData = ListTransactionsData;

    fn document() -> &'static str {
        r#"
        query ListTransactions($filter: TransactionFilter, $limit: Int, $offset: Int) {
            transactions(filter: $filter, limit: $limit, offset: $offset) {
                id
                stellar_account
                amount
                asset_code
                status
                created_at
                updated_at
                anchor_transaction_id
                memo
                memo_type
            }
        }
        "#
    }
}

// ── query { settlement(id: ID!): Settlement! } ────────────────────────────────

/// Typed variables for `GetSettlementQuery`.
#[derive(Debug, Clone, Serialize)]
pub struct GetSettlementVariables {
    pub id: String,
}

/// Response data for `GetSettlementQuery`.
#[derive(Debug, Clone, Deserialize)]
pub struct GetSettlementData {
    pub settlement: GqlSettlement,
}

/// Typed GraphQL operation: fetch a single settlement by ID.
pub struct GetSettlementQuery;

impl GraphQLOperation for GetSettlementQuery {
    type Variables = GetSettlementVariables;
    type ResponseData = GetSettlementData;

    fn document() -> &'static str {
        r#"
        query GetSettlement($id: ID!) {
            settlement(id: $id) {
                id
                asset_code
                total_amount
                tx_count
                period_start
                period_end
                status
                created_at
                updated_at
                dispute_reason
            }
        }
        "#
    }
}

// ── query { settlements(...): [Settlement!]! } ────────────────────────────────

/// Typed variables for `ListSettlementsQuery`.
#[derive(Debug, Default, Clone, Serialize)]
pub struct ListSettlementsVariables {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i32>,
}

/// Response data for `ListSettlementsQuery`.
#[derive(Debug, Clone, Deserialize)]
pub struct ListSettlementsData {
    pub settlements: Vec<GqlSettlement>,
}

/// Typed GraphQL operation: list settlements.
pub struct ListSettlementsQuery;

impl GraphQLOperation for ListSettlementsQuery {
    type Variables = ListSettlementsVariables;
    type ResponseData = ListSettlementsData;

    fn document() -> &'static str {
        r#"
        query ListSettlements($limit: Int, $offset: Int) {
            settlements(limit: $limit, offset: $offset) {
                id
                asset_code
                total_amount
                tx_count
                period_start
                period_end
                status
                created_at
                updated_at
                dispute_reason
            }
        }
        "#
    }
}

// ── mutation { forceCompleteTransaction(id: ID!): Transaction! } ──────────────

/// Typed variables for `ForceCompleteTransactionMutation`.
#[derive(Debug, Clone, Serialize)]
pub struct ForceCompleteTransactionVariables {
    pub id: String,
}

/// Response data for `ForceCompleteTransactionMutation`.
#[derive(Debug, Clone, Deserialize)]
pub struct ForceCompleteTransactionData {
    pub force_complete_transaction: GqlTransaction,
}

/// Typed GraphQL operation: force-complete a transaction.
pub struct ForceCompleteTransactionMutation;

impl GraphQLOperation for ForceCompleteTransactionMutation {
    type Variables = ForceCompleteTransactionVariables;
    type ResponseData = ForceCompleteTransactionData;

    fn document() -> &'static str {
        r#"
        mutation ForceCompleteTransaction($id: ID!) {
            forceCompleteTransaction(id: $id) {
                id
                stellar_account
                amount
                asset_code
                status
                created_at
                updated_at
            }
        }
        "#
    }
}

// ── mutation { replayDlq(id: ID!): Boolean! } ─────────────────────────────────

/// Typed variables for `ReplayDlqMutation`.
#[derive(Debug, Clone, Serialize)]
pub struct ReplayDlqVariables {
    pub id: String,
}

/// Response data for `ReplayDlqMutation`.
#[derive(Debug, Clone, Deserialize)]
pub struct ReplayDlqData {
    pub replay_dlq: bool,
}

/// Typed GraphQL operation: replay a dead-letter-queue entry.
pub struct ReplayDlqMutation;

impl GraphQLOperation for ReplayDlqMutation {
    type Variables = ReplayDlqVariables;
    type ResponseData = ReplayDlqData;

    fn document() -> &'static str {
        r#"
        mutation ReplayDlq($id: ID!) {
            replayDlq(id: $id)
        }
        "#
    }
}

// ── Typed response wrapper ────────────────────────────────────────────────────

/// A typed GraphQL response wrapping `data` and optional `errors`.
///
/// Returned by [`crate::resources::graphql::GraphQL::execute`].
#[derive(Debug, Clone, Deserialize)]
pub struct TypedGraphQLResponse<D> {
    pub data: Option<D>,
    #[serde(default)]
    pub errors: Vec<GraphQLResponseError>,
}

/// A single GraphQL application-level error in a typed response.
#[derive(Debug, Clone, Deserialize)]
pub struct GraphQLResponseError {
    pub message: String,
    #[serde(default)]
    pub extensions: Option<serde_json::Value>,
}

impl<D> TypedGraphQLResponse<D> {
    /// Returns `true` when the response contains at least one error.
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    /// Returns the first error message, if any.
    pub fn first_error(&self) -> Option<&str> {
        self.errors.first().map(|e| e.message.as_str())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_transaction_document_contains_required_fields() {
        let doc = GetTransactionQuery::document();
        assert!(doc.contains("transaction(id: $id)"), "missing field selector");
        assert!(doc.contains("stellar_account"), "missing field");
        assert!(doc.contains("status"), "missing field");
    }

    #[test]
    fn list_transactions_document_contains_filter_and_limit() {
        let doc = ListTransactionsQuery::document();
        assert!(doc.contains("$filter: TransactionFilter"), "missing filter variable");
        assert!(doc.contains("$limit: Int"), "missing limit variable");
        assert!(doc.contains("transactions(filter: $filter"), "missing resolver call");
    }

    #[test]
    fn get_settlement_document_contains_required_fields() {
        let doc = GetSettlementQuery::document();
        assert!(doc.contains("settlement(id: $id)"));
        assert!(doc.contains("total_amount"));
        assert!(doc.contains("tx_count"));
    }

    #[test]
    fn force_complete_transaction_document_is_mutation() {
        let doc = ForceCompleteTransactionMutation::document();
        assert!(doc.contains("mutation ForceCompleteTransaction"));
        assert!(doc.contains("forceCompleteTransaction(id: $id)"));
    }

    #[test]
    fn transaction_filter_skips_none_fields() {
        let f = TransactionFilter {
            status: Some("pending".into()),
            asset_code: None,
            stellar_account: None,
        };
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"status\":\"pending\""), "status must be present");
        assert!(!json.contains("asset_code"), "None fields must be skipped");
    }

    #[test]
    fn typed_response_has_errors_returns_true_when_errors_present() {
        let resp: TypedGraphQLResponse<serde_json::Value> = TypedGraphQLResponse {
            data: None,
            errors: vec![GraphQLResponseError {
                message: "some error".into(),
                extensions: None,
            }],
        };
        assert!(resp.has_errors());
        assert_eq!(resp.first_error(), Some("some error"));
    }

    #[test]
    fn typed_response_has_errors_returns_false_when_no_errors() {
        let resp: TypedGraphQLResponse<serde_json::Value> = TypedGraphQLResponse {
            data: Some(serde_json::json!({"ok": true})),
            errors: vec![],
        };
        assert!(!resp.has_errors());
        assert_eq!(resp.first_error(), None);
    }

    #[test]
    fn gql_transaction_deserializes_from_json() {
        let json = serde_json::json!({
            "id": "abc-123",
            "stellar_account": "GABC123",
            "amount": "100.00",
            "asset_code": "USD",
            "status": "pending",
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z"
        });
        let tx: GqlTransaction = serde_json::from_value(json).unwrap();
        assert_eq!(tx.id, "abc-123");
        assert_eq!(tx.status, "pending");
        assert!(tx.anchor_transaction_id.is_none());
    }

    #[test]
    fn list_settlements_variables_serializes_with_defaults() {
        let vars = ListSettlementsVariables::default();
        let json = serde_json::to_string(&vars).unwrap();
        // Both optional fields are None → empty object
        assert_eq!(json, "{}", "default vars should serialize to empty object");
    }
}
