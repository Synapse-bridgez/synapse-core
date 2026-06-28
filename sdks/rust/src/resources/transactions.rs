use crate::client::SynapseClient;
use crate::error::SynapseError;
use crate::models::{ListParams, SearchParams, Transaction, TransactionList, TransactionSearch};

pub struct Transactions<'a> {
    pub(crate) client: &'a SynapseClient,
}

impl<'a> Transactions<'a> {
    /// Fetch a single transaction by its UUID.
    ///
    /// Returns [`SynapseError::NotFound`] when the ID does not exist so callers
    /// can distinguish a missing record from other failure modes without
    /// inspecting raw HTTP status codes.
    ///
    /// # Errors
    /// - [`SynapseError::NotFound`] – no transaction with this ID exists (HTTP 404).
    /// - [`SynapseError::Api`] – server returned another non-success status.
    /// - [`SynapseError::Http`] – network error.
    /// - [`SynapseError::Decode`] – response body is not valid JSON.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use synapse_sdk::{SynapseClient, SynapseError};
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let client = SynapseClient::new("https://api.example.com", "your-api-key");
    ///
    /// match client.transactions().get("550e8400-e29b-41d4-a716-446655440000").await {
    ///     Ok(tx) => println!("status: {}", tx.status),
    ///     Err(SynapseError::NotFound(msg)) => eprintln!("not found: {}", msg),
    ///     Err(e) => eprintln!("error: {}", e),
    /// }
    /// # }
    /// ```
    pub async fn get(&self, id: &str) -> Result<Transaction, SynapseError> {
        let path = format!("/transactions/{}", id);
        match self.client.get::<Transaction>(&path).await {
            Err(SynapseError::Api {
                status: 404,
                message,
            }) => Err(SynapseError::NotFound(message)),
            other => other,
        }
    }

    /// List transactions with optional cursor-based pagination and date filters.
    ///
    /// Pass an [`ListParams`] value to control which page to retrieve. All
    /// fields are optional; omit them to use server defaults (25 records,
    /// forward order, no date filter).
    ///
    /// Cursors are opaque — always use `meta.next_cursor` from a previous
    /// response. Never construct or modify a cursor manually; an invalid or
    /// expired cursor returns [`SynapseError::InvalidCursor`] and must not be
    /// retried as-is.
    ///
    /// # Errors
    /// - [`SynapseError::InvalidCursor`] – the cursor is malformed or expired (HTTP 400).
    /// - [`SynapseError::Api`] – server returned another non-success status.
    /// - [`SynapseError::Http`] – network error.
    /// - [`SynapseError::Decode`] – response body is not valid JSON.
    ///
    /// # Example
    ///
    /// Fetch the first page, then follow `next_cursor`. An invalid or expired
    /// cursor must be surfaced to the caller — never silently retried with the
    /// same cursor:
    ///
    /// ```no_run
    /// use synapse_sdk::{ListParams, SynapseClient, SynapseError};
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let client = SynapseClient::new("https://api.example.com", "your-api-key");
    ///
    /// // First page: 50 records since the start of the year.
    /// let first = client
    ///     .transactions()
    ///     .list(ListParams {
    ///         limit: Some(50),
    ///         from_date: Some("2024-01-01T00:00:00Z".to_string()),
    ///         ..Default::default()
    ///     })
    ///     .await
    ///     .unwrap();
    ///
    /// for tx in &first.data {
    ///     println!("{} {} {}", tx.id, tx.status, tx.amount);
    /// }
    ///
    /// // Next page, only if the server issued a cursor.
    /// if let Some(cursor) = first.meta.next_cursor {
    ///     match client
    ///         .transactions()
    ///         .list(ListParams { cursor: Some(cursor), ..Default::default() })
    ///         .await
    ///     {
    ///         Ok(next) => println!("page 2 has {} records", next.data.len()),
    ///         // Surface this clearly and stop — do NOT retry the same cursor.
    ///         Err(SynapseError::InvalidCursor(msg)) => {
    ///             eprintln!("cursor rejected, restart pagination: {}", msg)
    ///         }
    ///         Err(e) => eprintln!("error: {}", e),
    ///     }
    /// }
    /// # }
    /// ```
    pub async fn list(&self, params: ListParams) -> Result<TransactionList, SynapseError> {
        let limit_str = params.limit.map(|l| l.to_string());
        let mut query: Vec<(&str, &str)> = Vec::new();

        if let Some(ref v) = params.cursor {
            query.push(("cursor", v.as_str()));
        }
        if let Some(ref v) = limit_str {
            query.push(("limit", v.as_str()));
        }
        if let Some(ref v) = params.from_date {
            query.push(("from_date", v.as_str()));
        }
        if let Some(ref v) = params.to_date {
            query.push(("to_date", v.as_str()));
        }

        match self
            .client
            .get_query::<TransactionList>("/transactions", &query)
            .await
        {
            Err(SynapseError::Api {
                status: 400,
                message,
            }) if message.contains("cursor") => Err(SynapseError::InvalidCursor(message)),
            other => other,
        }
    }

    /// Search transactions by filter, returning a single page of matches.
    ///
    /// Calls `GET /transactions/search` with any of the [`SearchParams`]
    /// fields supplied; omitted fields leave that dimension unfiltered. Uses
    /// the standard public client (`X-API-Key`).
    ///
    /// A search that matches nothing is **not** an error: it returns a
    /// [`TransactionSearch`] with `total == 0` and an empty `results` page.
    /// Use `next_cursor` to page through larger result sets.
    ///
    /// # Errors
    /// - [`SynapseError::InvalidCursor`] – the cursor is malformed or expired (HTTP 400).
    /// - [`SynapseError::Api`] – server returned another non-success status.
    /// - [`SynapseError::Http`] – network error.
    /// - [`SynapseError::Decode`] – response body is not valid JSON.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use synapse_sdk::{SearchParams, SynapseClient};
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let client = SynapseClient::new("https://api.example.com", "your-api-key");
    ///
    /// let filters = SearchParams {
    ///     status: Some("completed".to_string()),
    ///     asset_code: Some("USD".to_string()),
    ///     min_amount: Some("10.00".to_string()),
    ///     ..Default::default()
    /// };
    ///
    /// let page = client.transactions().search(filters).await.unwrap();
    /// println!("{} total matches, {} on this page", page.total, page.results.len());
    /// # }
    /// ```
    pub async fn search(&self, filters: SearchParams) -> Result<TransactionSearch, SynapseError> {
        let limit_str = filters.limit.map(|l| l.to_string());
        let mut query: Vec<(&str, &str)> = Vec::new();

        if let Some(ref v) = filters.status {
            query.push(("status", v.as_str()));
        }
        if let Some(ref v) = filters.asset_code {
            query.push(("asset_code", v.as_str()));
        }
        if let Some(ref v) = filters.min_amount {
            query.push(("min_amount", v.as_str()));
        }
        if let Some(ref v) = filters.max_amount {
            query.push(("max_amount", v.as_str()));
        }
        if let Some(ref v) = filters.from {
            query.push(("from", v.as_str()));
        }
        if let Some(ref v) = filters.to {
            query.push(("to", v.as_str()));
        }
        if let Some(ref v) = filters.stellar_account {
            query.push(("stellar_account", v.as_str()));
        }
        if let Some(ref v) = filters.cursor {
            query.push(("cursor", v.as_str()));
        }
        if let Some(ref v) = limit_str {
            query.push(("limit", v.as_str()));
        }

        match self
            .client
            .get_query::<TransactionSearch>("/transactions/search", &query)
            .await
        {
            Err(SynapseError::Api {
                status: 400,
                message,
            }) if message.contains("cursor") => Err(SynapseError::InvalidCursor(message)),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn transaction_body(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "stellar_account": "GABC1234567890123456789012345678901234567890123456789012",
            "amount": "100.00",
            "asset_code": "USD",
            "status": "pending",
            "created_at": "2024-01-15T10:00:00Z",
            "updated_at": "2024-01-15T10:00:00Z",
            "anchor_transaction_id": null,
            "callback_type": null,
            "callback_status": null,
            "settlement_id": null,
            "memo": null,
            "memo_type": null,
            "metadata": null
        })
    }

    #[tokio::test]
    async fn get_returns_transaction_on_200() {
        let server = MockServer::start().await;
        let tx_id = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("GET"))
            .and(path(format!("/transactions/{}", tx_id)))
            .and(header("X-API-Key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(transaction_body(tx_id)))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let result = client.transactions().get(tx_id).await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let tx = result.unwrap();
        assert_eq!(tx.id, tx_id);
        assert_eq!(tx.asset_code, "USD");
        assert_eq!(tx.status, "pending");
    }

    #[tokio::test]
    async fn get_returns_not_found_on_404() {
        let server = MockServer::start().await;
        let tx_id = "00000000-0000-0000-0000-000000000000";

        Mock::given(method("GET"))
            .and(path(format!("/transactions/{}", tx_id)))
            .respond_with(
                ResponseTemplate::new(404).set_body_string("Transaction 00000000 not found"),
            )
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let result = client.transactions().get(tx_id).await;

        assert!(
            matches!(result, Err(SynapseError::NotFound(_))),
            "expected NotFound, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn search_returns_page_on_200() {
        let server = MockServer::start().await;
        let tx_id = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("GET"))
            .and(path("/transactions/search"))
            .and(header("X-API-Key", "test-key"))
            .and(query_param("status", "pending"))
            .and(query_param("asset_code", "USD"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total": 1,
                "results": [transaction_body(tx_id)],
                "next_cursor": "next-page-token"
            })))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let filters = SearchParams {
            status: Some("pending".to_string()),
            asset_code: Some("USD".to_string()),
            ..Default::default()
        };
        let result = client.transactions().search(filters).await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let page = result.unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.results.len(), 1);
        assert_eq!(page.results[0].id, tx_id);
        assert_eq!(page.next_cursor.as_deref(), Some("next-page-token"));
    }

    #[tokio::test]
    async fn search_returns_empty_page_on_zero_matches() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/transactions/search"))
            .and(query_param("status", "nonexistent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total": 0,
                "results": []
            })))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let filters = SearchParams {
            status: Some("nonexistent".to_string()),
            ..Default::default()
        };
        let result = client.transactions().search(filters).await;

        assert!(
            result.is_ok(),
            "zero matches must be an empty page, not an error: {:?}",
            result
        );
        let page = result.unwrap();
        assert_eq!(page.total, 0);
        assert!(page.results.is_empty());
        assert!(page.next_cursor.is_none());
    }
}
