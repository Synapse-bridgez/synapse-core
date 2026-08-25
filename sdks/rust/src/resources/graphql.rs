use crate::client::SynapseClient;
use crate::error::SynapseError;
use crate::models::{GraphQLRequest, GraphQLResponse};

/// Access the `POST /graphql` endpoint.
pub struct GraphQL<'a> {
    pub(crate) client: &'a SynapseClient,
}

impl<'a> GraphQL<'a> {
    /// Send a raw GraphQL query and return the parsed response.
    ///
    /// Uses the standard public client (`X-API-Key`). A successful HTTP 200
    /// response that contains an `errors` array is surfaced as
    /// [`SynapseError::GraphQL`] — distinct from transport/network errors —
    /// so callers can handle application-level GraphQL failures separately.
    ///
    /// # Errors
    /// - [`SynapseError::GraphQL`] – HTTP 200 but the response contained an
    ///   `errors` array (application-level GraphQL error).
    /// - [`SynapseError::Api`] – server returned a non-success HTTP status.
    /// - [`SynapseError::Network`] – transport/network failure.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use synapse_sdk::SynapseClient;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let client = SynapseClient::new("https://api.example.com", "your-api-key");
    ///
    /// let resp = client
    ///     .graphql()
    ///     .query("{ transactions { id status } }", None)
    ///     .await
    ///     .unwrap();
    ///
    /// println!("{:?}", resp.data);
    /// # }
    /// ```
    pub async fn query(
        &self,
        query: impl Into<String>,
        variables: Option<serde_json::Value>,
    ) -> Result<GraphQLResponse, SynapseError> {
        let body = GraphQLRequest {
            query: query.into(),
            variables,
        };
        let resp: GraphQLResponse = self.client.post("/graphql", body).await?;
        if !resp.errors.is_empty() {
            let messages: Vec<&str> = resp.errors.iter().map(|e| e.message.as_str()).collect();
            return Err(SynapseError::GraphQL(messages.join("; ")));
        }
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn query_returns_data_on_200() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(header("X-API-Key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "transactions": [{ "id": "abc", "status": "pending" }] }
            })))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let result = client
            .graphql()
            .query("{ transactions { id status } }", None)
            .await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let resp = result.unwrap();
        assert!(resp.data.is_some());
        assert!(resp.errors.is_empty());
    }

    #[tokio::test]
    async fn query_surfaces_graphql_errors_on_200_with_errors_array() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "errors": [{ "message": "Unsupported GraphQL query" }]
            })))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let result = client.graphql().query("{ unknown }", None).await;

        assert!(
            matches!(result, Err(SynapseError::GraphQL(_))),
            "expected GraphQL error variant, got: {:?}",
            result
        );
    }
}
