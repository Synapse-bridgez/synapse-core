//! Outbound swap lifecycle notifications for tenant callback URLs.
//!
//! Tenants are pushed swap progress (quoted, accepted, filled, settled, failed,
//! refunded) instead of polling order endpoints. This module holds the pure
//! parts of that contract: the event set, the payload shape, request signing,
//! and callback URL validation. Queueing, retries, dead-lettering and endpoint
//! registration build on top of these.
//!
//! # Payload contract
//!
//! ```json
//! {
//!   "id": "swap:<transaction_id>:swap.settled",
//!   "type": "swap.settled",
//!   "sequence": 42,
//!   "created_at": "2026-10-19T12:00:00Z",
//!   "transaction_id": "<uuid>",
//!   "data": { ... }
//! }
//! ```
//!
//! `id` is stable per transaction and event, so receivers can deduplicate.
//! `sequence` is the outbox sequence number; deliveries to one endpoint are
//! ordered by it.
//!
//! # Signing
//!
//! Requests carry `X-Webhook-Timestamp` (Unix seconds) and
//! `X-Webhook-Signature: sha256=<hex>`. The HMAC-SHA256 is keyed by the
//! endpoint secret and computed over `{timestamp}.{body}`. Receivers verify
//! with [`crate::cache::webhook::verify_signature`] and
//! [`crate::cache::webhook::validate_timestamp`], the same scheme used for
//! inbound anchor callbacks.

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::Serialize;
use serde_json::Value;
use sha2::Sha256;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use url::{Host, Url};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// Upper bound on a serialized notification body.
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;

/// Swap state transitions a tenant can subscribe to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SwapLifecycleEvent {
    Quoted,
    Accepted,
    Filled,
    Settled,
    Failed,
    Refunded,
}

impl SwapLifecycleEvent {
    /// Every lifecycle event, for validating subscription lists.
    pub const ALL: [SwapLifecycleEvent; 6] = [
        SwapLifecycleEvent::Quoted,
        SwapLifecycleEvent::Accepted,
        SwapLifecycleEvent::Filled,
        SwapLifecycleEvent::Settled,
        SwapLifecycleEvent::Failed,
        SwapLifecycleEvent::Refunded,
    ];

    /// Wire name carried in the payload `type` field and subscription lists.
    pub fn event_type(self) -> &'static str {
        match self {
            SwapLifecycleEvent::Quoted => "swap.quoted",
            SwapLifecycleEvent::Accepted => "swap.accepted",
            SwapLifecycleEvent::Filled => "swap.filled",
            SwapLifecycleEvent::Settled => "swap.settled",
            SwapLifecycleEvent::Failed => "swap.failed",
            SwapLifecycleEvent::Refunded => "swap.refunded",
        }
    }
}

/// Errors from building, signing, or validating a swap notification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NotificationError {
    #[error("invalid callback URL: {0}")]
    InvalidUrl(String),
    #[error("callback URL must use https")]
    InsecureScheme,
    #[error("callback URL targets a private or reserved address")]
    BlockedAddress,
    #[error("payload is {size} bytes, limit is {max}")]
    PayloadTooLarge { size: usize, max: usize },
    #[error("webhook secret is empty")]
    EmptySecret,
    #[error("payload serialization failed: {0}")]
    Serialization(String),
}

/// One swap lifecycle notification in the documented payload shape.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SwapWebhookPayload {
    /// Stable per transaction and event; receivers deduplicate on this.
    pub id: String,
    #[serde(rename = "type")]
    pub event_type: &'static str,
    /// Outbox sequence number; per-endpoint ordering follows this.
    pub sequence: i64,
    pub created_at: DateTime<Utc>,
    pub transaction_id: Uuid,
    pub data: Value,
}

impl SwapWebhookPayload {
    pub fn new(
        event: SwapLifecycleEvent,
        transaction_id: Uuid,
        sequence: i64,
        created_at: DateTime<Utc>,
        data: Value,
    ) -> Self {
        Self {
            id: format!("swap:{transaction_id}:{}", event.event_type()),
            event_type: event.event_type(),
            sequence,
            created_at,
            transaction_id,
            data,
        }
    }

    /// Serialize the payload to the exact bytes that get signed and sent.
    pub fn to_body(&self) -> Result<Vec<u8>, NotificationError> {
        let body =
            serde_json::to_vec(self).map_err(|e| NotificationError::Serialization(e.to_string()))?;
        if body.len() > MAX_PAYLOAD_BYTES {
            return Err(NotificationError::PayloadTooLarge {
                size: body.len(),
                max: MAX_PAYLOAD_BYTES,
            });
        }
        Ok(body)
    }
}

/// Compute the `X-Webhook-Signature` header value for a body sent at `timestamp`.
pub fn sign_payload(
    secret: &[u8],
    timestamp: u64,
    body: &[u8],
) -> Result<String, NotificationError> {
    if secret.is_empty() {
        return Err(NotificationError::EmptySecret);
    }
    let mut mac =
        HmacSha256::new_from_slice(secret).map_err(|_| NotificationError::EmptySecret)?;
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    Ok(format!("sha256={}", hex::encode(mac.finalize().into_bytes())))
}

/// Validate a tenant-supplied callback URL before any request is made to it.
///
/// Requires `https`, no embedded credentials, and a host that is not a
/// loopback, private, link-local, or otherwise reserved address, nor a
/// single-label or internal-looking hostname. This checks the URL as written;
/// the delivery layer must also re-check the addresses the hostname resolves
/// to, to guard against DNS rebinding.
pub fn validate_callback_url(raw: &str) -> Result<Url, NotificationError> {
    let url = Url::parse(raw).map_err(|e| NotificationError::InvalidUrl(e.to_string()))?;
    if url.scheme() != "https" {
        return Err(NotificationError::InsecureScheme);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(NotificationError::InvalidUrl(
            "credentials are not allowed".into(),
        ));
    }
    let blocked = match url.host() {
        None => return Err(NotificationError::InvalidUrl("missing host".into())),
        Some(Host::Ipv4(ip)) => is_blocked_ip(IpAddr::V4(ip)),
        Some(Host::Ipv6(ip)) => is_blocked_ip(IpAddr::V6(ip)),
        Some(Host::Domain(domain)) => is_reserved_hostname(domain),
    };
    if blocked {
        return Err(NotificationError::BlockedAddress);
    }
    Ok(url)
}

fn is_reserved_hostname(domain: &str) -> bool {
    let host = domain.trim_end_matches('.').to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || !host.contains('.')
}

fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_blocked_v4(v4),
            None => is_blocked_v6(v6),
        },
    }
}

fn is_blocked_v4(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || a == 0
        // RFC 1918: 10/8, 172.16/12, 192.168/16
        || a == 10
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        // Carrier-grade NAT, 100.64.0.0/10
        || (a == 100 && (64..=127).contains(&b))
}

fn is_blocked_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        // Unique local, fc00::/7
        || (first & 0xfe00) == 0xfc00
        // Link-local, fe80::/10
        || (first & 0xffc0) == 0xfe80
}

