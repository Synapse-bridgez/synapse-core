//! Tenant notification preferences, templates, and multi-channel delivery
//! planning for swap lifecycle events.
//!
//! The canonical event payload is never modified: rendering is a pure function
//! of `(template, event, channel)` that only produces the notification body, so
//! templates cannot suppress compliance fields. Rendering failures are returned
//! as [`DeliveryOutcome::Failed`] so they appear in the delivery log instead of
//! being dropped. Persistence: `migrations/20260928000002_swap_notification_settings.sql`.
//!
//! Default for a tenant with no saved preferences: opted in to every event
//! type on the `webhook` channel, delivered immediately (the pre-existing
//! behaviour). Email is opt-in only.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const MAX_TEMPLATE_BYTES: usize = 4096;
pub const MAX_RECIPIENTS: usize = 20;
/// Variables a template may reference; each resolves to a top-level scalar of the event.
pub const ALLOWED_VARIABLES: &[&str] = &[
    "event_type",
    "order_id",
    "tenant_id",
    "status",
    "from_asset",
    "to_asset",
    "amount",
    "reason",
];
/// Events that are never batched into a digest.
const CRITICAL_EVENTS: &[&str] = &["swap.failed", "swap.refunded", "swap.held"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Webhook,
    Email,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    Immediate,
    Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipients {
    pub emails: Vec<String>,
    pub webhook_urls: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationPreferences {
    /// Empty set means "all event types".
    pub subscribed_events: BTreeSet<String>,
    pub channels: BTreeSet<Channel>,
    pub mode: DeliveryMode,
    pub recipients: Recipients,
}

impl Default for NotificationPreferences {
    fn default() -> Self {
        Self {
            subscribed_events: BTreeSet::new(),
            channels: BTreeSet::from([Channel::Webhook]),
            mode: DeliveryMode::Immediate,
            recipients: Recipients {
                emails: vec![],
                webhook_urls: vec![],
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NotificationError {
    #[error("template exceeds {MAX_TEMPLATE_BYTES} bytes")]
    TemplateTooLarge,
    #[error("template has an unterminated or empty placeholder")]
    MalformedPlaceholder,
    #[error("unknown template variable `{0}`")]
    UnknownVariable(String),
    #[error("event is missing variable `{0}`")]
    MissingValue(String),
    #[error("invalid recipient: {0}")]
    InvalidRecipient(String),
    #[error("more than {MAX_RECIPIENTS} recipients")]
    TooManyRecipients,
}

/// Safe built-in template used until a tenant customizes an event type.
pub fn default_template(_event_type: &str) -> &'static str {
    "Swap {{order_id}}: {{event_type}} ({{status}})"
}

fn placeholders(template: &str) -> Result<Vec<&str>, NotificationError> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let end = after
            .find("}}")
            .ok_or(NotificationError::MalformedPlaceholder)?;
        let name = after[..end].trim();
        if name.is_empty() {
            return Err(NotificationError::MalformedPlaceholder);
        }
        out.push(name);
        rest = &after[end + 2..];
    }
    Ok(out)
}

/// Save-time validation: size cap, well-formed placeholders, known variables only.
pub fn validate_template(template: &str) -> Result<(), NotificationError> {
    if template.len() > MAX_TEMPLATE_BYTES {
        return Err(NotificationError::TemplateTooLarge);
    }
    for name in placeholders(template)? {
        if !ALLOWED_VARIABLES.contains(&name) {
            return Err(NotificationError::UnknownVariable(name.to_string()));
        }
    }
    Ok(())
}

fn escape(value: &str, channel: Channel) -> String {
    match channel {
        Channel::Email => value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;"),
        // Body is embedded in a JSON string for webhooks.
        Channel::Webhook => {
            let quoted = Value::String(value.to_string()).to_string();
            quoted[1..quoted.len() - 1].to_string()
        }
    }
}

/// Pure single-pass renderer: substituted values are never re-scanned, so a
/// value containing `{{...}}` cannot expand further.
pub fn render(
    template: &str,
    event: &Value,
    channel: Channel,
) -> Result<String, NotificationError> {
    validate_template(template)?;
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find("}}").expect("validated");
        let name = after[..end].trim();
        let value = match event.get(name) {
            Some(Value::String(s)) => s.clone(),
            Some(v @ (Value::Number(_) | Value::Bool(_))) => v.to_string(),
            _ => return Err(NotificationError::MissingValue(name.to_string())),
        };
        out.push_str(&escape(&value, channel));
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

fn is_valid_email(e: &str) -> bool {
    let mut parts = e.split('@');
    matches!((parts.next(), parts.next(), parts.next()), (Some(l), Some(d), None)
        if !l.is_empty() && d.contains('.') && !d.starts_with('.') && !d.ends_with('.')
            && !e.chars().any(|c| c.is_whitespace() || c.is_control()))
}

fn is_safe_webhook_url(u: &str) -> bool {
    let Some(rest) = u.strip_prefix("https://") else {
        return false;
    };
    let host_port = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host_port.contains('@') || host_port.is_empty() {
        return false;
    }
    let host = host_port
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let private = host == "localhost"
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.starts_with('[')
        || host.starts_with("127.")
        || host.starts_with("10.")
        || host.starts_with("192.168.")
        || host.starts_with("169.254.")
        || host.starts_with("0.")
        || (host.starts_with("172.")
            && host
                .split('.')
                .nth(1)
                .and_then(|o| o.parse::<u8>().ok())
                .is_some_and(|o| (16..=31).contains(&o)));
    !private
}

pub fn validate_recipients(r: &Recipients) -> Result<(), NotificationError> {
    if r.emails.len() + r.webhook_urls.len() > MAX_RECIPIENTS {
        return Err(NotificationError::TooManyRecipients);
    }
    if let Some(e) = r.emails.iter().find(|e| !is_valid_email(e)) {
        return Err(NotificationError::InvalidRecipient(e.clone()));
    }
    if let Some(u) = r.webhook_urls.iter().find(|u| !is_safe_webhook_url(u)) {
        return Err(NotificationError::InvalidRecipient(u.clone()));
    }
    Ok(())
}

impl NotificationPreferences {
    pub fn validate(&self) -> Result<(), NotificationError> {
        validate_recipients(&self.recipients)
    }

    /// Channels that should receive `event_type` right now. Digest-mode tenants
    /// still get critical events immediately; other events return empty here
    /// (they are queued for the digest by the caller).
    pub fn immediate_channels(&self, event_type: &str) -> Vec<Channel> {
        if !self.subscribed_events.is_empty() && !self.subscribed_events.contains(event_type) {
            return vec![];
        }
        if self.mode == DeliveryMode::Digest && !CRITICAL_EVENTS.contains(&event_type) {
            return vec![];
        }
        self.channels.iter().copied().collect()
    }
}

/// Result recorded in the delivery log for each channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Ready {
        channel: Channel,
        body: String,
    },
    Failed {
        channel: Channel,
        error: NotificationError,
    },
}

/// Plan deliveries for one event. `canonical` is returned to the caller
/// untouched; `template` is the tenant's template for this event type, if any.
pub fn plan_deliveries(
    prefs: &NotificationPreferences,
    template: Option<&str>,
    event_type: &str,
    canonical: &Value,
) -> Vec<DeliveryOutcome> {
    let template = template.unwrap_or_else(|| default_template(event_type));
    prefs
        .immediate_channels(event_type)
        .into_iter()
        .map(|channel| match render(template, canonical, channel) {
            Ok(body) => DeliveryOutcome::Ready { channel, body },
            Err(error) => DeliveryOutcome::Failed { channel, error },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event() -> Value {
        json!({"event_type": "swap.completed", "order_id": "o-1", "status": "<b>done</b>", "amount": 12, "reason": "{{order_id}}"})
    }

    #[test]
    fn renders_and_escapes_per_channel() {
        let t = "{{order_id}}: {{status}} {{amount}}";
        assert_eq!(
            render(t, &event(), Channel::Email).unwrap(),
            "o-1: &lt;b&gt;done&lt;/b&gt; 12"
        );
        let ev = json!({"order_id": "a\"b", "status": "x", "amount": 1});
        assert_eq!(
            render("{{order_id}}", &ev, Channel::Webhook).unwrap(),
            "a\\\"b"
        );
    }

    #[test]
    fn values_are_not_re_expanded() {
        assert_eq!(
            render("{{reason}}", &event(), Channel::Email).unwrap(),
            "{{order_id}}"
        );
    }

    #[test]
    fn validation_errors() {
        assert_eq!(
            validate_template("{{nope}}"),
            Err(NotificationError::UnknownVariable("nope".into()))
        );
        assert_eq!(
            validate_template("{{order_id"),
            Err(NotificationError::MalformedPlaceholder)
        );
        assert_eq!(
            validate_template("{{ }}"),
            Err(NotificationError::MalformedPlaceholder)
        );
        assert_eq!(
            validate_template(&"x".repeat(MAX_TEMPLATE_BYTES + 1)),
            Err(NotificationError::TemplateTooLarge)
        );
        assert!(validate_template(default_template("swap.completed")).is_ok());
    }

    #[test]
    fn recipient_validation_blocks_ssrf_and_caps_size() {
        let ok = Recipients {
            emails: vec!["a@b.co".into()],
            webhook_urls: vec!["https://hooks.example.com/x".into()],
        };
        assert!(validate_recipients(&ok).is_ok());
        for bad in [
            "http://example.com",
            "https://localhost/x",
            "https://10.0.0.1/",
            "https://172.20.1.1/",
            "https://u@evil.com/",
            "https://[::1]/",
        ] {
            let r = Recipients {
                emails: vec![],
                webhook_urls: vec![bad.into()],
            };
            assert!(validate_recipients(&r).is_err(), "{bad}");
        }
        let many = Recipients {
            emails: (0..21).map(|i| format!("u{i}@b.co")).collect(),
            webhook_urls: vec![],
        };
        assert_eq!(
            validate_recipients(&many),
            Err(NotificationError::TooManyRecipients)
        );
    }

    #[test]
    fn preferences_take_effect_on_next_event() {
        let mut p = NotificationPreferences::default();
        assert_eq!(
            p.immediate_channels("swap.completed"),
            vec![Channel::Webhook]
        );
        p.subscribed_events.insert("swap.failed".into());
        p.channels.insert(Channel::Email);
        assert!(p.immediate_channels("swap.completed").is_empty());
        assert_eq!(p.immediate_channels("swap.failed").len(), 2);
        p.mode = DeliveryMode::Digest;
        p.subscribed_events.insert("swap.completed".into());
        assert!(p.immediate_channels("swap.completed").is_empty());
        assert_eq!(p.immediate_channels("swap.failed").len(), 2);
    }

    #[test]
    fn malformed_template_is_a_visible_failure_not_a_drop() {
        let p = NotificationPreferences::default();
        let out = plan_deliveries(&p, Some("{{bogus}}"), "swap.completed", &event());
        assert!(matches!(
            &out[..],
            [DeliveryOutcome::Failed {
                channel: Channel::Webhook,
                ..
            }]
        ));
        let missing = plan_deliveries(&p, None, "swap.completed", &json!({"order_id": "x"}));
        assert!(matches!(&missing[..], [DeliveryOutcome::Failed { .. }]));
    }
}
