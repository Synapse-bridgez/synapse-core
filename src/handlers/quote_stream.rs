//! Authenticated swap quote transports. Both use the REST TenantContext extractor.
use axum::{
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, Query, State},
    http::HeaderMap,
    response::{sse::{Event, KeepAlive, Sse}, IntoResponse, Response},
    Extension,
};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use std::{convert::Infallible, time::{Duration, Instant}};
use crate::{ApiState, error::{AppError, SwapApiError}, tenant::TenantContext,
    swap::streaming::{AssetPair, QuoteBroker, QuoteConnection}};

#[derive(Default, Deserialize)]
pub struct StreamQuery {
    /// JSON-encoded array of {source,destination} objects.
    pairs: Option<String>,
    cursor: Option<String>,
}

impl StreamQuery {
    fn pairs(&self) -> Result<Vec<AssetPair>, SwapApiError> {
        match &self.pairs {
            Some(value) if value.len() <= 16384 => serde_json::from_str(value).map_err(|_| SwapApiError::Validation),
            Some(_) => Err(SwapApiError::LimitExceeded),
            None => Ok(vec![]),
        }
    }
}

fn credential(headers: &HeaderMap) -> Result<String, SwapApiError> {
    let value = headers.get("X-API-Key").or_else(|| headers.get("Authorization"))
        .and_then(|v| v.to_str().ok()).ok_or(SwapApiError::Unauthorized)?;
    Ok(value.strip_prefix("Bearer ").unwrap_or(value).to_owned())
}

async fn still_authorized(state: &ApiState, tenant: uuid::Uuid, key: &str, deadline: Duration) -> bool {
    // Re-query storage: catches grace-period expiry, key revocation and inactive
    // tenants during long-lived sessions, rather than trusting the config cache.
    matches!(tokio::time::timeout(deadline, crate::db::queries::lookup_api_key(&state.app_state.db, key)).await, Ok(Ok(Some(owner))) if owner == tenant)
}

pub async fn websocket(
    State(state): State<ApiState>,
    Extension(broker): Extension<QuoteBroker>,
    tenant: Result<TenantContext, AppError>,
    query: Result<Query<StreamQuery>, axum::extract::rejection::QueryRejection>,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> Result<Response, SwapApiError> {
    let tenant = tenant.map_err(|error| SwapApiError::from_status(error.into_response().status()))?;
    if state.app_state.readiness.is_draining() { return Err(SwapApiError::Unavailable); }
    let Query(query) = query.map_err(|_| SwapApiError::Validation)?;
    let ws = ws.map_err(|_| SwapApiError::Validation)?;
    let key = credential(&headers)?;
    let pairs = query.pairs()?;
    let connection = broker.connect(tenant.tenant_id, pairs.clone(), query.cursor.as_deref())?;
    Ok(ws.max_message_size(16384).max_frame_size(16384)
        .on_upgrade(move |socket| run_socket(socket, state, tenant.tenant_id, key, broker, connection, pairs)).into_response())
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Command { Subscribe { pairs: Vec<AssetPair> }, Unsubscribe { pairs: Vec<AssetPair> }, Ping }

async fn run_socket(socket: WebSocket, state: ApiState, tenant: uuid::Uuid, key: String,
    broker: QuoteBroker, mut connection: QuoteConnection, mut pairs: Vec<AssetPair>) {
    let (mut sink, mut source) = socket.split();
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    let mut last_input = Instant::now();
    let mut auth_checked = Instant::now() - broker.config.auth_interval;
    loop {
        let message = tokio::select! {
            biased;
            _ = interval.tick() => {
                if last_input.elapsed() >= broker.config.idle_timeout || state.app_state.readiness.is_draining() { break; }
                if auth_checked.elapsed() >= broker.config.auth_interval {
                    if !still_authorized(&state, tenant, &key, broker.config.send_timeout).await { break; }
                    auth_checked = Instant::now();
                }
                continue;
            }
            event = connection.recv() => {
                let Some(event) = event else { break; };
                match serde_json::to_string(&event) { Ok(json) => Message::Text(json), Err(_) => break }
            }
            input = source.next() => {
                let Some(Ok(input)) = input else { break; };
                last_input = Instant::now();
                match input {
                    Message::Text(text) => {
                        let command = serde_json::from_str::<Command>(&text);
                        let result = match command {
                            Ok(Command::Subscribe { pairs: additions }) => {
                                let mut next = pairs.clone();
                                for pair in additions { if !next.contains(&pair) { next.push(pair); } }
                                connection.set_pairs(next.clone()).map(|_| { pairs = next; })
                            }
                            Ok(Command::Unsubscribe { pairs: removals }) => {
                                let next: Vec<_> = pairs.iter().filter(|p| !removals.contains(p)).cloned().collect();
                                connection.set_pairs(next.clone()).map(|_| { pairs = next; })
                            }
                            Ok(Command::Ping) => Ok(()),
                            Err(_) => Err(SwapApiError::Validation),
                        };
                        match result {
                            Ok(()) => Message::Text(serde_json::json!({"type":"subscribed", "pairs":pairs}).to_string()),
                            Err(error) => Message::Text(serde_json::json!({"type":"error", "problem":error.problem()}).to_string()),
                        }
                    }
                    Message::Ping(data) => Message::Pong(data),
                    Message::Pong(_) => continue,
                    Message::Close(_) => break,
                    Message::Binary(_) => break,
                }
            }
        };
        if !matches!(tokio::time::timeout(broker.config.send_timeout, sink.send(message)).await, Ok(Ok(()))) { break; }
    }
    let _ = tokio::time::timeout(broker.config.send_timeout, sink.send(Message::Close(None))).await;
    // RAII removes this connection even if upgrade task is cancelled.
}

pub async fn events(
    State(state): State<ApiState>,
    Extension(broker): Extension<QuoteBroker>,
    tenant: Result<TenantContext, AppError>,
    query: Result<Query<StreamQuery>, axum::extract::rejection::QueryRejection>,
    headers: HeaderMap,
) -> Result<Response, SwapApiError> {
    let tenant = tenant.map_err(|error| SwapApiError::from_status(error.into_response().status()))?;
    if state.app_state.readiness.is_draining() { return Err(SwapApiError::Unavailable); }
    let Query(query) = query.map_err(|_| SwapApiError::Validation)?;
    let key = credential(&headers)?;
    let cursor = match headers.get("Last-Event-ID") {
        Some(value) => Some(value.to_str().map_err(|_| SwapApiError::Validation)?),
        None => query.cursor.as_deref(),
    };
    let mut connection = broker.connect(tenant.tenant_id, query.pairs()?, cursor)?;
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        let mut last_event = Instant::now();
        let mut auth_checked = Instant::now() - broker.config.auth_interval;
        loop {
            tokio::select! {
                biased;
                _ = interval.tick() => {
                    if last_event.elapsed() >= broker.config.idle_timeout || state.app_state.readiness.is_draining() { break; }
                    if auth_checked.elapsed() >= broker.config.auth_interval {
                        if !still_authorized(&state, tenant.tenant_id, &key, broker.config.send_timeout).await { break; }
                        auth_checked = Instant::now();
                    }
                }
                event = connection.recv() => {
                    let Some(event) = event else { break; };
                    let name = match event.kind {
                        crate::swap::streaming::QuoteEventKind::Created => "quote_created",
                        crate::swap::streaming::QuoteEventKind::Updated => "quote_updated",
                        crate::swap::streaming::QuoteEventKind::Expired => "quote_expired",
                    };
                    let Ok(data) = serde_json::to_string(&event) else { break; };
                    last_event = Instant::now();
                    let frame = Event::default().id(event.cursor).event(name).data(data);
                    if !matches!(tokio::time::timeout(broker.config.send_timeout, sender.send(frame)).await, Ok(Ok(()))) { break; }
                }
            }
        }
    });
    let guard = AbortOnDrop(task);
    let stream = async_stream::stream! {
        let _guard = guard;
        while let Some(event) = receiver.recv().await { yield Ok::<_, Infallible>(event); }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))).into_response())
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) { self.0.abort(); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_follow_rest_header_precedence() {
        let mut headers = HeaderMap::new();
        assert!(matches!(credential(&headers), Err(SwapApiError::Unauthorized)));
        headers.insert("Authorization", "Bearer first".parse().unwrap());
        assert_eq!(credential(&headers).unwrap(), "first");
        headers.insert("X-API-Key", "second".parse().unwrap());
        assert_eq!(credential(&headers).unwrap(), "second");
    }

    #[test]
    fn infrastructure_and_quota_auth_errors_keep_their_classification() {
        for (error, expected) in [
            (AppError::Internal("private connection details".into()), SwapApiError::Internal),
            (AppError::RateLimitExceeded, SwapApiError::LimitExceeded),
            (AppError::InvalidApiKey, SwapApiError::Unauthorized),
        ] {
            assert_eq!(SwapApiError::from_status(error.into_response().status()), expected);
        }
    }

    #[test]
    fn commands_cannot_supply_tenant_identity() {
        assert!(serde_json::from_str::<Command>(r#"{"type":"subscribe","pairs":[],"tenant_id":"forged"}"#).is_err());
        assert!(matches!(serde_json::from_str::<Command>(r#"{"type":"unsubscribe","pairs":[]}"#), Ok(Command::Unsubscribe { .. })));
    }
}
