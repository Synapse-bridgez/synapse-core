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
    InvalidRecipi
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
