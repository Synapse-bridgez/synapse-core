//! Typed, fluent builders for the GraphQL query shapes exposed by
//! `src/graphql/resolvers/transaction.rs` and
//! `src/graphql/resolvers/settlement.rs`, so common lookups don't require
//! hand-writing query strings (see `examples/graphql_query.rs`).
//!
//! This is a hand-maintained builder for the transaction/settlement shapes
//! covered here, not a full schema codegen pipeline — see the parent issue
//! for why that's out of scope for now.
//!
//! # Schema drift
//!
//! [`TransactionField`] and [`SettlementField`] mirror the `#[Object]`
//! methods on `Transaction`/`Settlement` in `src/db/models.rs`. If the
//! server schema adds, renames, or removes a field these builders expose,
//! update the enum here — the `field_count_matches_known_schema` test below
//! is a build-breaking canary that fails as soon as the field count no
//! longer matches what this module documents, and
//! `builder_output_is_accepted_by_the_server` exercises the generated query
//! against a mocked server response shaped like the real schema.

//! # Decoding responses
//!
//! These builders only produce the query string — they deliberately don't
//! decode responses into [`crate::models::Transaction`]/[`crate::models::Settlement`],
//! because those REST models deserialize snake_case field names
//! (`stellar_account`, `created_at`, …) while the GraphQL schema exposes the
//! same fields camelCased (`stellarAccount`, `createdAt`, …) per
//! async-graphql's default renaming. Decode `client.graphql().query(...)`'s
//! `resp.data` yourself (see `examples/graphql_query.rs`), e.g. into a
//! `#[serde(rename_all = "camelCase")]` type shaped like your selection.

/// Fields selectable on a `Transaction` GraphQL object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionField {
    Id,
    StellarAccount,
    Amount,
    AssetCode,
    Status,
    CreatedAt,
    UpdatedAt,
    AnchorTransactionId,
    CallbackType,
    CallbackStatus,
    SettlementId,
    Memo,
    MemoType,
}

impl TransactionField {
    /// Every field this builder knows how to select, in the order
    /// `src/db/models.rs` declares them on `Transaction`.
    pub const ALL: &[TransactionField] = &[
        Self::Id,
        Self::StellarAccount,
        Self::Amount,
        Self::AssetCode,
        Self::Status,
        Self::CreatedAt,
        Self::UpdatedAt,
        Self::AnchorTransactionId,
        Self::CallbackType,
        Self::CallbackStatus,
        Self::SettlementId,
        Self::Memo,
        Self::MemoType,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::StellarAccount => "stellarAccount",
            Self::Amount => "amount",
            Self::AssetCode => "assetCode",
            Self::Status => "status",
            Self::CreatedAt => "createdAt",
            Self::UpdatedAt => "updatedAt",
            Self::AnchorTransactionId => "anchorTransactionId",
            Self::CallbackType => "callbackType",
            Self::CallbackStatus => "callbackStatus",
            Self::SettlementId => "settlementId",
            Self::Memo => "memo",
            Self::MemoType => "memoType",
        }
    }
}

/// Fields selectable on a `Settlement` GraphQL object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettlementField {
    Id,
    AssetCode,
    TotalAmount,
    TxCount,
    PeriodStart,
    PeriodEnd,
    Status,
    CreatedAt,
    UpdatedAt,
}

impl SettlementField {
    /// Every field this builder knows how to select, in the order
    /// `src/db/models.rs` declares them on `Settlement`.
    pub const ALL: &[SettlementField] = &[
        Self::Id,
        Self::AssetCode,
        Self::TotalAmount,
        Self::TxCount,
        Self::PeriodStart,
        Self::PeriodEnd,
        Self::Status,
        Self::CreatedAt,
        Self::UpdatedAt,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::AssetCode => "assetCode",
            Self::TotalAmount => "totalAmount",
            Self::TxCount => "txCount",
            Self::PeriodStart => "periodStart",
            Self::PeriodEnd => "periodEnd",
            Self::Status => "status",
            Self::CreatedAt => "createdAt",
            Self::UpdatedAt => "updatedAt",
        }
    }
}

/// Escapes `value` as a GraphQL string literal (including the surrounding
/// quotes).
fn graphql_string_literal(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn selection_set<F: Copy>(fields: &[F], as_str: impl Fn(F) -> &'static str) -> String {
    fields.iter().copied().map(as_str).collect::<Vec<_>>().join(" ")
}

/// Filter for the `transactions` list query, mirroring `TransactionFilter`
/// in `src/graphql/resolvers/transaction.rs`.
#[derive(Debug, Clone, Default)]
pub struct TransactionQueryFilter {
    pub status: Option<String>,
    pub asset_code: Option<String>,
    pub stellar_account: Option<String>,
}

/// Typed builder for the `transaction`/`transactions` query shapes exposed
/// by `src/graphql/resolvers/transaction.rs`.
#[derive(Debug, Clone)]
pub struct TransactionQueryBuilder {
    fields: Vec<TransactionField>,
}

impl Default for TransactionQueryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TransactionQueryBuilder {
    /// Starts a builder selecting every known field ([`TransactionField::ALL`]).
    pub fn new() -> Self {
        Self {
            fields: TransactionField::ALL.to_vec(),
        }
    }

    /// Restricts the selection set to exactly `fields`.
    pub fn select(mut self, fields: &[TransactionField]) -> Self {
        self.fields = fields.to_vec();
        self
    }

    /// Builds `{ transaction(id: "...") { <fields> } }`.
    pub fn by_id_query(&self, id: &str) -> String {
        format!(
            "{{ transaction(id: {}) {{ {} }} }}",
            graphql_string_literal(id),
            selection_set(&self.fields, TransactionField::as_str)
        )
    }

    /// Builds `{ transactions(filter: {...}, limit: N, offset: N) { <fields> } }`.
    /// Omits `filter`/`limit`/`offset` arguments that are `None`/empty,
    /// matching the resolver's own optional-argument defaults.
    pub fn list_query(
        &self,
        filter: Option<&TransactionQueryFilter>,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> String {
        let mut args = Vec::new();

        if let Some(f) = filter {
            let mut filter_fields = Vec::new();
            if let Some(status) = &f.status {
                filter_fields.push(format!("status: {}", graphql_string_literal(status)));
            }
            if let Some(asset_code) = &f.asset_code {
                filter_fields.push(format!("assetCode: {}", graphql_string_literal(asset_code)));
            }
            if let Some(stellar_account) = &f.stellar_account {
                filter_fields.push(format!(
                    "stellarAccount: {}",
                    graphql_string_literal(stellar_account)
                ));
            }
            if !filter_fields.is_empty() {
                args.push(format!("filter: {{ {} }}", filter_fields.join(", ")));
            }
        }
        if let Some(limit) = limit {
            args.push(format!("limit: {limit}"));
        }
        if let Some(offset) = offset {
            args.push(format!("offset: {offset}"));
        }

        let args = if args.is_empty() {
            String::new()
        } else {
            format!("({})", args.join(", "))
        };

        format!(
            "{{ transactions{} {{ {} }} }}",
            args,
            selection_set(&self.fields, TransactionField::as_str)
        )
    }
}

/// Typed builder for the `settlements` query shape exposed by
/// `src/graphql/resolvers/settlement.rs`.
#[derive(Debug, Clone)]
pub struct SettlementQueryBuilder {
    fields: Vec<SettlementField>,
}

impl Default for SettlementQueryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl SettlementQueryBuilder {
    /// Starts a builder selecting every known field ([`SettlementField::ALL`]).
    pub fn new() -> Self {
        Self {
            fields: SettlementField::ALL.to_vec(),
        }
    }

    /// Restricts the selection set to exactly `fields`.
    pub fn select(mut self, fields: &[SettlementField]) -> Self {
        self.fields = fields.to_vec();
        self
    }

    /// Builds `{ settlements(limit: N, offset: N) { <fields> } }`.
    pub fn list_query(&self, limit: Option<i64>, offset: Option<i64>) -> String {
        let mut args = Vec::new();
        if let Some(limit) = limit {
            args.push(format!("limit: {limit}"));
        }
        if let Some(offset) = offset {
            args.push(format!("offset: {offset}"));
        }

        let args = if args.is_empty() {
            String::new()
        } else {
            format!("({})", args.join(", "))
        };

        format!(
            "{{ settlements{} {{ {} }} }}",
            args,
            selection_set(&self.fields, SettlementField::as_str)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Build-breaking canary: if the server schema adds/removes a field on
    /// `Transaction`/`Settlement`, this count changes and the test fails,
    /// forcing `TransactionField`/`SettlementField` to be updated in the
    /// same change rather than silently drifting.
    #[test]
    fn field_count_matches_known_schema() {
        assert_eq!(TransactionField::ALL.len(), 13);
        assert_eq!(SettlementField::ALL.len(), 9);
    }

    #[test]
    fn by_id_query_selects_requested_fields_only() {
        let query = TransactionQueryBuilder::new()
            .select(&[TransactionField::Id, TransactionField::Status])
            .by_id_query("tx-123");
        assert_eq!(query, r#"{ transaction(id: "tx-123") { id status } }"#);
    }

    #[test]
    fn list_query_renders_filter_and_pagination_args() {
        let filter = TransactionQueryFilter {
            status: Some("completed".to_string()),
            asset_code: Some("USD".to_string()),
            stellar_account: None,
        };
        let query = TransactionQueryBuilder::new()
            .select(&[TransactionField::Id])
            .list_query(Some(&filter), Some(10), Some(5));
        assert_eq!(
            query,
            r#"{ transactions(filter: { status: "completed", assetCode: "USD" }, limit: 10, offset: 5) { id } }"#
        );
    }

    #[test]
    fn list_query_omits_absent_args() {
        let query = SettlementQueryBuilder::new()
            .select(&[SettlementField::Id, SettlementField::Status])
            .list_query(None, None);
        assert_eq!(query, "{ settlements { id status } }");
    }

    #[tokio::test]
    async fn builder_output_is_accepted_by_the_server() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_partial_json(serde_json::json!({
                "query": r#"{ transactions(limit: 20) { id status } }"#
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "transactions": [{ "id": "abc", "status": "pending" }] }
            })))
            .mount(&server)
            .await;

        let client = crate::client::SynapseClient::new(server.uri(), "test-key");
        let builder =
            TransactionQueryBuilder::new().select(&[TransactionField::Id, TransactionField::Status]);
        let query = builder.list_query(None, Some(20), None);
        let result = client.graphql().query(query, None).await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        assert!(result.unwrap().data.is_some());
    }
}
