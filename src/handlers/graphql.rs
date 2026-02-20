use async_graphql::http::{playground_source, GraphQLPlaygroundConfig};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::{
    extract::State,
    response::{Html, IntoResponse},
};
use crate::ApiState;

pub async fn graphql_handler(
    State(state): State<ApiState>,
    req: GraphQLRequest,
) -> GraphQLResponse {
    state.graphql_schema.execute(req.into_inner()).await.into()
}

pub async fn subscription_handler(
    State(state): State<ApiState>,
    protocol: async_graphql_axum::GraphQLProtocol,
    upgrade: axum::extract::WebSocketUpgrade,
) -> impl IntoResponse {
    upgrade
        .protocols(async_graphql::http::ALL_WEBSOCKET_PROTOCOLS)
        .on_upgrade(move |stream| {
            async_graphql_axum::GraphQLWebSocket::new(stream, state.graphql_schema, protocol).serve()
        })
}

pub async fn graphql_playground() -> impl IntoResponse {
    Html(playground_source(GraphQLPlaygroundConfig::new("/graphql")))
}
