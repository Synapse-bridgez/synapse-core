//! The live `/graphql` handler.
//!
//! # Incident note (Part C)
//!
//! Until this fix, this file did not execute the schema `lib.rs` builds and
//! stores in `ApiState.graphql_schema` at all. It was a hand-rolled
//! substring-matcher (`query.contains("transactions{")`, etc.) implementing
//! three hardcoded operations by hand, with everything else — including
//! the real GraphQL schema, its depth/complexity/alias limits, rate
//! limiting, redacted error handling, and every real resolver — silently
//! unreachable from production traffic. The correct wiring survived,
//! orphaned, in a tracked-but-never-compiled `graphql.rs.bak` file
//! alongside it; `git log` on the live file's last substantive change is a
//! large merge-conflict-resolution commit, consistent with this being an
//! accidental loss during conflict resolution rather than a deliberate
//! rewrite.
//!
//! This is a broader process gap, not just a code gap: a `.bak` file
//! containing the correct implementation sat in git next to a materially
//! different live replacement, and nothing caught the divergence — no
//! compiler error (both are valid Rust), no test failure (the stand-in's
//! own narrow behavior passed its own narrow tests). Reviewing a
//! merge-conflict resolution needs to specifically ask "does the mounted
//! handler still call the thing it's named after," not just "does it
//! compile and pass tests" — that question is what actually catches this
//! class of regression. See the CI check added alongside this fix
//! (`scripts/check-graphql-handler-wired.sh`) for a cheap, mechanical guard
//! against exactly this happening again silently.
use crate::ApiState;
use async_graphql::http::{playground_source, GraphQLPlaygroundConfig};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::{
    extract::State,
    response::{Html, IntoResponse},
};

/// The live `/graphql` endpoint: actually executes `state.graphql_schema`
/// against the incoming query, rather than pattern-matching on query text.
pub async fn graphql_handler(
    State(state): State<ApiState>,
    req: GraphQLRequest,
) -> GraphQLResponse {
    GraphQLResponse(async_graphql::BatchResponse::Single(
        state.graphql_schema.execute(req.into_inner()).await,
    ))
}

/// GraphQL-over-WebSocket subscription handler. Not currently mounted in
/// `lib.rs` — the application already has its own bespoke WebSocket
/// real-time channel (`handlers/ws.rs`), and wiring a second, GraphQL-native
/// subscription transport was not asked for by the issue this restores.
/// Kept here, compiled and ready, for whenever that's actually wanted.
#[allow(dead_code)]
pub async fn subscription_handler(
    State(state): State<ApiState>,
    protocol: async_graphql_axum::GraphQLProtocol,
    upgrade: axum::extract::WebSocketUpgrade,
) -> impl IntoResponse {
    upgrade
        .protocols(async_graphql::http::ALL_WEBSOCKET_PROTOCOLS)
        .on_upgrade(move |stream| {
            async_graphql_axum::GraphQLWebSocket::new(stream, state.graphql_schema, protocol)
                .serve()
        })
}

/// GraphQL Playground UI. Not mounted in `lib.rs` — the issue this restores
/// explicitly notes no playground/introspection is exposed in production
/// today, and this fix does not change that default.
#[allow(dead_code)]
pub async fn graphql_playground() -> impl IntoResponse {
    Html(playground_source(GraphQLPlaygroundConfig::new("/graphql")))
}
