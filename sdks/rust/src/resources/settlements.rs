use crate::client::SynapseClient;
use crate::error::SynapseError;
use crate::models::{Settlement, SettlementList, SettlementParams};

use crate::models::{Settlement, SettlementList};

/// Handle to the `settlements` resource.
pub struct Settlements<'a> {
    pub(crate) client: &'a SynapseClient,
}

impl<'a> Settlements<'a> {
    /// Fetch a single settlement by its UUID.
    ///
    /// Returns [`SynapseError::NotFound`] when the ID does not exist so callers
    /// can distinguish a missing record from other failure modes without
    /// inspecting raw HTTP status codes.
    ///
    /// # Errors
    /// - [`SynapseError::NotFound`] – no settlement with this ID exists (HTTP 404).
    /// - [`SynapseError::Api`] – server returned another non-success status.
    /// - [`SynapseError::Http`] – network error.
    /// - [`SynapseError::Decode`] – response body is not valid JSON.
    /// List settlements with optional cursor-based pagination.
    ///
    /// Calls `GET /settlements`. All query parameters are optional; omit them
    /// to accept the server defaults (10 records per page, forward order).
    ///
    /// Cursors are opaque — always pass `next_cursor` from a previous response.
    /// Never construct or modify a cursor manually; an invalid or expired cursor
    /// returns [`SynapseError::InvalidCursor`] and must not be retried as-is.
    ///
    /// # Errors
    /// - [`SynapseError::InvalidCursor`] – cursor is malformed or expired (HTTP 400).
    /// - [`SynapseError::Api`] – server returned another non-success status.
    /// - [`SynapseError::Network`] – network error before a response was received.
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
    /// match client.settlements().get("550e8400-e29b-41d4-a716-446655440000").await {
    ///     Ok(s) => println!("status: {}  amount: {}", s.status, s.total_amount),
    ///     Err(SynapseError::NotFound(msg)) => eprintln!("not found: {}", msg),
    ///     Err(e) => eprintln!("error: {}", e),
    /// }
    /// # }
    /// ```
    pub async fn get(&self, id: &str) -> Result<Settlement, SynapseError> {
        let path = format!("/settlements/{}", id);
        match self.client.get::<Settlement>(&path).await {
            Err(SynapseError::Api {
                status: 404,
                message,
            }) => Err(SynapseError::NotFound(message)),
    /// let mut cursor: Option<String> = None;
    /// loop {
    ///     let page = client
    ///         .settlements()
    ///         .list(cursor.as_deref(), Some(20))
    ///         .await
    ///         .unwrap();
    ///
    ///     for s in &page.settlements {
    ///         println!("{} {} {}", s.id, s.status, s.total_amount);
    ///     }
    ///
    ///     if page.meta.has_more {
    ///         cursor = page.meta.next_cursor;
    ///     } else {
    ///         break;
    ///     }
    /// }
    /// # }
    /// ```
    pub async fn list(
        &self,
        cursor: Option<&str>,
        limit: Option<i64>,
    ) -> Result<SettlementList, SynapseError> {
        let limit_str;
        let mut query: Vec<(&str, &str)> = Vec::new();
        if let Some(c) = cursor {
            query.push(("cursor", c));
        }
        if let Some(l) = limit {
            limit_str = l.to_string();
            query.push(("limit", &limit_str));
        }
        match self
            .client
            .get_query::<SettlementList>("/settlements", &query)
            .await
        {
            Err(SynapseError::Api {
                status: 400,
                message,
            }) if message.contains("cursor") => Err(SynapseError::InvalidCursor(message)),
            other => other,
        }
    }

    /// List settlements with optional cursor-based pagination and direction.
    ///
    /// Pass a [`SettlementParams`] value to control which page to retrieve. All
    /// fields are optional; omit them to use server defaults (10 records,
    /// forward order).
    ///
    /// Cursors are opaque — always use `next_cursor` from a previous response.
    /// Never construct or modify a cursor manually; an invalid or expired cursor
    /// returns [`SynapseError::InvalidCursor`] and must not be retried as-is.
    ///
    /// # Errors
    /// - [`SynapseError::InvalidCursor`] – the cursor is malformed or expired (HTTP 400).
    /// - [`SynapseError::Api`] – server returned another non-success status.
    /// - [`SynapseError::Http`] – network error.
    /// - [`SynapseError::Decode`] – response body is not valid JSON.
    ///
    /// # Example
    ///
    /// Fetch the first page, then follow `next_cursor`:
    ///
    /// ```no_run
    /// use synapse_sdk::{SettlementParams, SynapseClient, SynapseError};
    /// Fetch a single settlement by its UUID.
    ///
    /// Returns [`SynapseError::NotFound`] when the ID does not exist, allowing
    /// callers to distinguish a missing record from transport errors without
    /// inspecting raw HTTP status codes.
    ///
    /// # Errors
    /// - [`SynapseError::NotFound`] – no settlement with this ID exists (HTTP 404).
    /// - [`SynapseError::Api`] – server returned another non-success status.
    /// - [`SynapseError::Network`] – network error before a response was received.
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
    /// // First page: 25 records, forward order.
    /// let first = client
    ///     .settlements()
    ///     .list(SettlementParams {
    ///         limit: Some(25),
    ///         ..Default::default()
    ///     })
    ///     .await
    ///     .unwrap();
    ///
    /// for s in &first.settlements {
    ///     println!("{} {} {}", s.id, s.status, s.total_amount);
    /// }
    ///
    /// // Next page, only if the server issued a cursor.
    /// if let Some(cursor) = first.next_cursor {
    ///     match client
    ///         .settlements()
    ///         .list(SettlementParams {
    ///             cursor: Some(cursor),
    ///             ..Default::default()
    ///         })
    ///         .await
    ///     {
    ///         Ok(next) => println!("page 2 has {} records", next.settlements.len()),
    ///         Err(SynapseError::InvalidCursor(msg)) => {
    ///             eprintln!("cursor rejected, restart pagination: {}", msg)
    ///         }
    ///         Err(e) => eprintln!("error: {}", e),
    ///     }
    /// }
    /// # }
    /// ```
    pub async fn list(&self, params: SettlementParams) -> Result<SettlementList, SynapseError> {
        let limit_str = params.limit.map(|l| l.to_string());
        let mut query: Vec<(&str, &str)> = Vec::new();

        if let Some(ref v) = params.cursor {
            query.push(("cursor", v.as_str()));
        }
        if let Some(ref v) = limit_str {
            query.push(("limit", v.as_str()));
        }
        if let Some(ref v) = params.direction {
            query.push(("direction", v.as_str()));
        }

        match self
            .client
            .get_query::<SettlementList>("/settlements", &query)
            .await
        {
            Err(SynapseError::Api {
                status: 400,
                message,
            }) if message.contains("cursor") => Err(SynapseError::InvalidCursor(message)),
    /// match client.settlements().get("550e8400-e29b-41d4-a716-446655440000").await {
    ///     Ok(s) => println!("status: {}", s.status),
    ///     // A missing settlement ID is distinguishable from a transport error.
    ///     Err(SynapseError::NotFound(msg)) => eprintln!("not found: {}", msg),
    ///     Err(e) => eprintln!("error: {}", e),
    /// }
    /// # }
    /// ```
    pub async fn get(&self, id: &str) -> Result<Settlement, SynapseError> {
        let path = format!("/settlements/{}", id);
        match self.client.get::<Settlement>(&path).await {
            Err(SynapseError::Api {
                status: 404,
                message,
            }) => Err(SynapseError::NotFound(message)),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn settlement_body(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "asset_code": "USD",
            "total_amount": "5000.00",
            "tx_count": 42,
            "period_start": "2024-01-01T00:00:00Z",
            "period_end": "2024-01-31T23:59:59Z",
            "status": "completed",
            "created_at": "2024-02-01T10:00:00Z",
            "updated_at": "2024-02-01T10:00:00Z",
            "tx_count": 10,
            "period_start": "2024-01-01T00:00:00Z",
            "period_end": "2024-01-31T23:59:59Z",
            "status": "completed",
            "created_at": "2024-02-01T00:00:00Z",
            "updated_at": "2024-02-01T00:00:00Z",
            "dispute_reason": null,
            "original_total_amount": null,
            "reviewed_by": null,
            "reviewed_at": null
        })
    }

    #[tokio::test]
    async fn get_returns_settlement_on_200() {
        let server = MockServer::start().await;
        let id = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("GET"))
            .and(path(format!("/settlements/{}", id)))
            .and(header("X-API-Key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(settlement_body(id)))
    async fn list_returns_settlements_on_200() {
        let server = MockServer::start().await;
        let id = "550e8400-e29b-41d4-a716-446655440001";

        Mock::given(method("GET"))
            .and(path("/settlements"))
            .and(header("X-API-Key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "settlements": [settlement_body(id)],
                "next_cursor": null,
                "has_more": false
            })))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let result = client.settlements().get(id).await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let s = result.unwrap();
        assert_eq!(s.id, id);
        assert_eq!(s.asset_code, "USD");
        assert_eq!(s.total_amount, "5000.00");
        assert_eq!(s.status, "completed");
    }

    #[tokio::test]
    async fn get_returns_not_found_on_404() {
        let server = MockServer::start().await;
        let id = "00000000-0000-0000-0000-000000000000";

        Mock::given(method("GET"))
            .and(path(format!("/settlements/{}", id)))
            .respond_with(ResponseTemplate::new(404).set_body_string("Settlement 00000000 not found"))
        let result = client.settlements().list(None, None).await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let page = result.unwrap();
        assert_eq!(page.settlements.len(), 1);
        assert_eq!(page.settlements[0].id, id);
        assert!(!page.meta.has_more);
    }

    #[tokio::test]
    async fn list_passes_cursor_and_limit() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/settlements"))
            .and(query_param("cursor", "tok"))
            .and(query_param("limit", "5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "settlements": [],
                "next_cursor": null,
                "has_more": false
            })))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let result = client.settlements().get(id).await;

        assert!(
            matches!(result, Err(SynapseError::NotFound(_))),
            "expected NotFound, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn list_returns_page_on_200() {
        let server = MockServer::start().await;
        let id = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("GET"))
            .and(path("/settlements"))
            .and(header("X-API-Key", "test-key"))
            .and(query_param("limit", "10"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "settlements": [settlement_body(id)],
                "next_cursor": "next-page-token",
                "has_more": true
            })))
        let result = client.settlements().list(Some("tok"), Some(5)).await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
    }

    #[tokio::test]
    async fn get_returns_settlement_on_200() {
        let server = MockServer::start().await;
        let id = "550e8400-e29b-41d4-a716-446655440002";

        Mock::given(method("GET"))
            .and(path(format!("/settlements/{}", id)))
            .and(header("X-API-Key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(settlement_body(id)))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let params = SettlementParams {
            limit: Some(10),
            ..Default::default()
        };
        let result = client.settlements().list(params).await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let page = result.unwrap();
        assert_eq!(page.settlements.len(), 1);
        assert_eq!(page.settlements[0].id, id);
        assert_eq!(page.next_cursor.as_deref(), Some("next-page-token"));
        assert!(page.has_more);
    }

    #[tokio::test]
    async fn list_handles_invalid_cursor() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/settlements"))
            .and(query_param("cursor", "bad-cursor"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_string("invalid cursor: bad-cursor"),
        let result = client.settlements().get(id).await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        assert_eq!(result.unwrap().id, id);
    }

    #[tokio::test]
    async fn get_returns_not_found_on_404() {
        let server = MockServer::start().await;
        let id = "00000000-0000-0000-0000-000000000000";

        Mock::given(method("GET"))
            .and(path(format!("/settlements/{}", id)))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_string(format!("Settlement {} not found", id)),
            )
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let params = SettlementParams {
            cursor: Some("bad-cursor".to_string()),
            ..Default::default()
        };
        let result = client.settlements().list(params).await;

        assert!(
            matches!(result, Err(SynapseError::InvalidCursor(_))),
            "expected InvalidCursor, got: {:?}",
        let result = client.settlements().get(id).await;

        assert!(
            matches!(result, Err(SynapseError::NotFound(_))),
            "expected NotFound, got: {:?}",
            result
        );
    }
}
