//! Tenant-scoped quote cache and non-blocking stream fan-out.
use crate::error::SwapApiError;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{collections::{HashMap, HashSet, VecDeque}, sync::Arc, time::Duration};
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct QuoteStreamConfig {
    pub max_connections: usize,
    pub max_pairs: usize,
    pub buffer: usize,
    pub history: usize,
    pub max_quotes: usize,
    pub max_tenants: usize,
    pub idle_timeout: Duration,
    pub auth_interval: Duration,
    pub send_timeout: Duration,
}

impl Default for QuoteStreamConfig {
    fn default() -> Self {
        Self { max_connections: 1024, max_pairs: 32, buffer: 128, history: 1024,
            max_quotes: 10000, max_tenants: 1000, idle_timeout: Duration::from_secs(300),
            auth_interval: Duration::from_secs(15), send_timeout: Duration::from_secs(5) }
    }
}

impl QuoteStreamConfig {
    pub fn from_env() -> Self {
        let mut value = Self::default();
        fn number(name: &str, default: usize) -> usize {
            std::env::var(name).ok().and_then(|v| v.parse().ok()).filter(|n| *n > 0).unwrap_or(default)
        }
        value.max_connections = number("QUOTE_STREAM_MAX_CONNECTIONS", value.max_connections);
        value.max_pairs = number("QUOTE_STREAM_MAX_PAIRS", value.max_pairs);
        value.buffer = number("QUOTE_STREAM_BUFFER", value.buffer);
        value.history = number("QUOTE_STREAM_HISTORY", value.history);
        value.max_quotes = number("QUOTE_STREAM_MAX_QUOTES", value.max_quotes);
        value.max_tenants = number("QUOTE_STREAM_MAX_TENANTS", value.max_tenants);
        value.idle_timeout = Duration::from_secs(number("QUOTE_STREAM_IDLE_SECONDS", 300) as u64);
        value.auth_interval = Duration::from_secs(number("QUOTE_STREAM_AUTH_SECONDS", 15) as u64);
        value.send_timeout = Duration::from_secs(number("QUOTE_STREAM_SEND_SECONDS", 5) as u64);
        value
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetPair { pub source: String, pub destination: String }

impl AssetPair {
    pub fn validate(&self) -> Result<(), SwapApiError> {
        let valid = |s: &str| !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_:./".contains(&b));
        if !valid(&self.source) || !valid(&self.destination) || self.source == self.destination {
            return Err(SwapApiError::Validation);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Quote {
    pub id: Uuid,
    pub pair: AssetPair,
    /// Decimal strings avoid floating-point rounding in client integrations.
    pub input_amount: String,
    pub output_amount: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuoteEventKind { Created, Updated, Expired }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QuoteEvent {
    pub cursor: String,
    pub sequence: u64,
    pub kind: QuoteEventKind,
    pub quote: Quote,
}

struct Subscriber {
    tenant: Uuid,
    pairs: HashSet<AssetPair>,
    sender: mpsc::Sender<QuoteEvent>,
    disconnected: watch::Sender<bool>,
}

#[derive(Default)]
struct TenantHistory { sequence: u64, events: VecDeque<QuoteEvent> }
struct Inner {
    epoch: Uuid,
    tenants: HashMap<Uuid, TenantHistory>,
    quotes: HashMap<(Uuid, Uuid), Quote>,
    subscribers: HashMap<Uuid, Subscriber>,
}

#[derive(Clone)]
pub struct QuoteBroker { inner: Arc<Mutex<Inner>>, pub config: QuoteStreamConfig }

impl QuoteBroker {
    pub fn new(mut config: QuoteStreamConfig) -> Self {
        config.buffer = config.buffer.max(1);
        config.history = config.history.max(1);
        config.auth_interval = config.auth_interval.max(Duration::from_secs(1));
        Self { inner: Arc::new(Mutex::new(Inner { epoch: Uuid::new_v4(), tenants: HashMap::new(), quotes: HashMap::new(), subscribers: HashMap::new() })), config }
    }

    /// The authoritative quote write entrypoint: update cache and publish atomically.
    /// Tenant comes from the trusted pricing caller, never from a stream client.
    pub fn publish(&self, tenant: Uuid, quote: Quote) -> Result<QuoteEvent, SwapApiError> {
        quote.pair.validate()?;
        for amount in [&quote.input_amount, &quote.output_amount] {
            if amount.len() > 128 || amount.parse::<bigdecimal::BigDecimal>().map(|n| n <= bigdecimal::BigDecimal::from(0)).unwrap_or(true) {
                return Err(SwapApiError::Validation);
            }
        }
        if quote.expires_at <= Utc::now() { return Err(SwapApiError::Expired); }
        let mut inner = self.inner.lock();
        self.ensure_tenant(&mut inner, tenant)?;
        let key = (tenant, quote.id);
        if let Some(old) = inner.quotes.get(&key) {
            if old.pair != quote.pair { return Err(SwapApiError::Validation); }
        } else if inner.quotes.len() >= self.config.max_quotes { return Err(SwapApiError::LimitExceeded); }
        let kind = if inner.quotes.insert(key, quote.clone()).is_some() { QuoteEventKind::Updated } else { QuoteEventKind::Created };
        Ok(self.emit(&mut inner, tenant, quote, kind))
    }

    pub fn snapshot(&self, tenant: Uuid, pairs: &[AssetPair]) -> Vec<Quote> {
        self.inner.lock().quotes.iter().filter(|((owner, _), q)| *owner == tenant && pairs.contains(&q.pair) && q.expires_at > Utc::now()).map(|(_, q)| q.clone()).collect()
    }

    pub fn expire(&self, now: DateTime<Utc>) {
        let mut inner = self.inner.lock();
        let expired: Vec<_> = inner.quotes.iter().filter(|(_, q)| q.expires_at <= now).map(|(key, q)| (*key, q.clone())).collect();
        for ((tenant, id), quote) in expired {
            inner.quotes.remove(&(tenant, id));
            self.emit(&mut inner, tenant, quote, QuoteEventKind::Expired);
        }
    }

    fn ensure_tenant(&self, inner: &mut Inner, tenant: Uuid) -> Result<(), SwapApiError> {
        if !inner.tenants.contains_key(&tenant) && inner.tenants.len() >= self.config.max_tenants { return Err(SwapApiError::LimitExceeded); }
        inner.tenants.entry(tenant).or_default();
        Ok(())
    }

    fn emit(&self, inner: &mut Inner, tenant: Uuid, quote: Quote, kind: QuoteEventKind) -> QuoteEvent {
        let history = inner.tenants.get_mut(&tenant).expect("registered quote tenant");
        history.sequence += 1;
        let event = QuoteEvent { cursor: format!("{}:{}", inner.epoch, history.sequence), sequence: history.sequence, kind, quote };
        history.events.push_back(event.clone());
        while history.events.len() > self.config.history { history.events.pop_front(); }
        inner.subscribers.retain(|_, sub| {
            if sub.tenant == tenant && sub.pairs.contains(&event.quote.pair) && sub.sender.try_send(event.clone()).is_err() {
                let _ = sub.disconnected.send(true);
                return false;
            }
            true
        });
        event
    }

    pub fn connect(&self, tenant: Uuid, pairs: Vec<AssetPair>, cursor: Option<&str>) -> Result<QuoteConnection, SwapApiError> {
        let pairs = self.validate_pairs(pairs)?;
        let mut inner = self.inner.lock();
        if inner.subscribers.len() >= self.config.max_connections { return Err(SwapApiError::LimitExceeded); }
        self.ensure_tenant(&mut inner, tenant)?;
        let replay = self.replay(&inner, tenant, &pairs, cursor)?;
        if replay.len() > self.config.buffer { return Err(SwapApiError::StaleQuote); }
        let (sender, receiver) = mpsc::channel(self.config.buffer);
        for event in replay { sender.try_send(event).map_err(|_| SwapApiError::StaleQuote)?; }
        let (disconnected, cancelled) = watch::channel(false);
        let id = Uuid::new_v4();
        inner.subscribers.insert(id, Subscriber { tenant, pairs, sender, disconnected });
        Ok(QuoteConnection { id, broker: self.clone(), receiver, cancelled })
    }

    fn validate_pairs(&self, pairs: Vec<AssetPair>) -> Result<HashSet<AssetPair>, SwapApiError> {
        if pairs.len() > self.config.max_pairs { return Err(SwapApiError::LimitExceeded); }
        for pair in &pairs { pair.validate()?; }
        Ok(pairs.into_iter().collect())
    }

    fn replay(&self, inner: &Inner, tenant: Uuid, pairs: &HashSet<AssetPair>, cursor: Option<&str>) -> Result<Vec<QuoteEvent>, SwapApiError> {
        let Some(cursor) = cursor else { return Ok(vec![]); };
        let (epoch, sequence) = cursor.split_once(':').ok_or(SwapApiError::Validation)?;
        let epoch = Uuid::parse_str(epoch).map_err(|_| SwapApiError::Validation)?;
        let sequence: u64 = sequence.parse().map_err(|_| SwapApiError::Validation)?;
        let history = &inner.tenants[&tenant];
        if epoch != inner.epoch || sequence > history.sequence || history.events.front().map(|first| sequence < first.sequence.saturating_sub(1)).unwrap_or(sequence != 0) {
            return Err(SwapApiError::StaleQuote);
        }
        Ok(history.events.iter().filter(|event| event.sequence > sequence && pairs.contains(&event.quote.pair)).cloned().collect())
    }

    pub fn active_connections(&self) -> usize { self.inner.lock().subscribers.len() }

    pub fn disconnect_all(&self) {
        let mut inner = self.inner.lock();
        for (_, subscriber) in inner.subscribers.drain() {
            let _ = subscriber.disconnected.send(true);
        }
    }
}

pub struct QuoteConnection {
    id: Uuid,
    broker: QuoteBroker,
    receiver: mpsc::Receiver<QuoteEvent>,
    cancelled: watch::Receiver<bool>,
}

impl QuoteConnection {
    pub async fn recv(&mut self) -> Option<QuoteEvent> {
        if *self.cancelled.borrow() { return None; }
        tokio::select! {
            biased;
            _ = self.cancelled.changed() => None,
            event = self.receiver.recv() => event,
        }
    }

    /// Replaces subscriptions. Resume is accepted only on initial connection:
    /// changing filters cannot replay behind already queued sequence numbers.
    pub fn set_pairs(&self, pairs: Vec<AssetPair>) -> Result<(), SwapApiError> {
        let pairs = self.broker.validate_pairs(pairs)?;
        let mut inner = self.broker.inner.lock();
        let sub = inner.subscribers.get_mut(&self.id).ok_or(SwapApiError::Expired)?;
        sub.pairs = pairs;
        Ok(())
    }
}

impl Drop for QuoteConnection {
    fn drop(&mut self) { self.broker.inner.lock().subscribers.remove(&self.id); }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> AssetPair { AssetPair { source: "USDC".into(), destination: "XLM".into() } }
    fn quote() -> Quote { Quote { id: Uuid::new_v4(), pair: pair(), input_amount: "10".into(), output_amount: "100".into(), expires_at: Utc::now() + chrono::Duration::minutes(1) } }

    #[tokio::test]
    async fn lifecycle_updates_expiry_and_tenant_isolation() {
        let broker = QuoteBroker::new(QuoteStreamConfig::default());
        let owner = Uuid::new_v4();
        let mut stream = broker.connect(owner, vec![pair()], None).unwrap();
        let mut other = broker.connect(Uuid::new_v4(), vec![pair()], None).unwrap();
        let mut q = quote();
        broker.publish(owner, q.clone()).unwrap();
        assert_eq!(stream.recv().await.unwrap().kind, QuoteEventKind::Created);
        assert!(other.receiver.try_recv().is_err());
        q.output_amount = "101".into();
        broker.publish(owner, q.clone()).unwrap();
        assert_eq!(stream.recv().await.unwrap().sequence, 2);
        broker.expire(q.expires_at);
        assert_eq!(stream.recv().await.unwrap().kind, QuoteEventKind::Expired);
        assert!(broker.snapshot(owner, &[pair()]).is_empty());
        drop(stream);
        drop(other);
        assert_eq!(broker.active_connections(), 0);
    }

    #[tokio::test]
    async fn replay_and_gap_detection_are_atomic_with_registration() {
        let broker = QuoteBroker::new(QuoteStreamConfig { history: 2, ..Default::default() });
        let tenant = Uuid::new_v4();
        let cursor = broker.publish(tenant, quote()).unwrap().cursor;
        let second = broker.publish(tenant, quote()).unwrap();
        let mut stream = broker.connect(tenant, vec![pair()], Some(&cursor)).unwrap();
        assert_eq!(stream.recv().await.unwrap().cursor, second.cursor);
        broker.publish(tenant, quote()).unwrap();
        assert_eq!(stream.recv().await.unwrap().sequence, 3);
        broker.publish(tenant, quote()).unwrap();
        assert!(matches!(broker.connect(tenant, vec![pair()], Some(&cursor)), Err(SwapApiError::StaleQuote)));
        let restarted = QuoteBroker::new(QuoteStreamConfig::default());
        assert!(matches!(restarted.connect(tenant, vec![pair()], Some(&cursor)), Err(SwapApiError::StaleQuote)));
    }

    #[tokio::test]
    async fn slow_subscribers_are_removed_without_blocking_fast_subscriber() {
        let broker = QuoteBroker::new(QuoteStreamConfig { buffer: 1, ..Default::default() });
        let tenant = Uuid::new_v4();
        let mut slow = broker.connect(tenant, vec![pair()], None).unwrap();
        let mut fast = broker.connect(tenant, vec![pair()], None).unwrap();
        broker.publish(tenant, quote()).unwrap();
        fast.recv().await.unwrap();
        broker.publish(tenant, quote()).unwrap();
        assert_eq!(broker.active_connections(), 1);
        assert!(slow.recv().await.is_none());
        assert_eq!(fast.recv().await.unwrap().sequence, 2);
        broker.disconnect_all();
        assert!(fast.recv().await.is_none());
        assert_eq!(broker.active_connections(), 0);
    }

    #[tokio::test]
    async fn many_subscribers_remain_bounded_and_fanout_responsive() {
        let broker = QuoteBroker::new(QuoteStreamConfig { max_connections: 512, buffer: 1, ..Default::default() });
        let tenant = Uuid::new_v4();
        let mut connections: Vec<_> = (0..512).map(|_| broker.connect(tenant, vec![pair()], None).unwrap()).collect();
        assert!(matches!(broker.connect(tenant, vec![pair()], None), Err(SwapApiError::LimitExceeded)));
        for _ in 0..20 {
            broker.publish(tenant, quote()).unwrap();
            for connection in &mut connections { assert!(connection.receiver.try_recv().is_ok()); }
        }
        drop(connections);
        assert_eq!(broker.active_connections(), 0);
    }

    #[tokio::test]
    async fn subscription_replacement_and_limits() {
        let broker = QuoteBroker::new(QuoteStreamConfig { max_pairs: 1, ..Default::default() });
        let tenant = Uuid::new_v4();
        let mut connection = broker.connect(tenant, vec![], None).unwrap();
        connection.set_pairs(vec![pair()]).unwrap();
        broker.publish(tenant, quote()).unwrap();
        connection.recv().await.unwrap();
        connection.set_pairs(vec![]).unwrap();
        broker.publish(tenant, quote()).unwrap();
        assert!(connection.receiver.try_recv().is_err());
        assert!(matches!(connection.set_pairs(vec![pair(), pair()]), Err(SwapApiError::LimitExceeded)));
    }
}
