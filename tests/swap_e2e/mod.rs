//! Deterministic end-to-end harness for the Swap Engine.
//!
//! [`SwapHarness::start`] spins up a [`SwapService`] behind its real HTTP
//! router (`swap::http::router`) with:
//!
//! * mock liquidity providers ([`MockLp`]) whose execution faults — failed
//!   fills, adverse drift, ignored `min_out` — are either scripted per call
//!   or drawn from a seeded PRNG;
//! * a mock price oracle ([`MockOracle`]) that can be repriced or taken down;
//! * a test database ([`TestDb`]): the in-memory store, or a throwaway
//!   Postgres container via testcontainers;
//! * a [`ManualClock`] and [`SequentialIdGenerator`], so ids and timestamps
//!   are identical on every run.
//!
//! Requests are driven through the router with `tower::ServiceExt::oneshot`
//! (no sockets, no ports), and every request/response pair plus every mock
//! call is appended to a transcript. Same seed ⇒ byte-identical transcript;
//! [`SwapHarness::digest`] hashes it so reproducibility is assertable.
//!
//! Reproducing a failure: the [`SeedGuard`] prints the seed and the tail of
//! the transcript when a test panics. Re-run with
//! `SWAP_E2E_SEED=<seed> cargo test --test swap_e2e_harness_test`.

#![allow(dead_code)]

use async_trait::async_trait;
use axum::body::Body;
use axum::http::Request;
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

use synapse_core::swap::asset::{Asset, AssetRegistry};
use synapse_core::swap::decimal::{Decimal, RATE_SCALE};
use synapse_core::swap::engine::SwapService;
use synapse_core::swap::fees::FeeSchedule;
use synapse_core::swap::http;
use synapse_core::swap::liquidity::{
    LegFill, LegOrder, LiquidityError, LiquidityProvider, Pair, PairQuote,
};
use synapse_core::swap::oracle::{OracleError, PriceOracle};
use synapse_core::swap::pricing::convert;
use synapse_core::swap::rounding::{Purpose, RoundingMode, RoundingPolicy};
use synapse_core::swap::routing::RouterConfig;
use synapse_core::swap::runtime::{ManualClock, SequentialIdGenerator};
use synapse_core::swap::store::{InMemorySwapStore, SwapStore};

/// Seed used when `SWAP_E2E_SEED` is not set.
pub const DEFAULT_SEED: u64 = 0x5EED_2026_0001;

/// Seed from `SWAP_E2E_SEED` (decimal or `0x` hex), else [`DEFAULT_SEED`].
pub fn seed_from_env() -> u64 {
    match std::env::var("SWAP_E2E_SEED") {
        Ok(s) => {
            let s = s.trim();
            let parsed = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                Some(hex) => u64::from_str_radix(hex, 16),
                None => s.parse(),
            };
            parsed.unwrap_or_else(|_| panic!("SWAP_E2E_SEED={s:?} is not a u64"))
        }
        Err(_) => DEFAULT_SEED,
    }
}

// ---------------------------------------------------------------------------
// Deterministic PRNG
// ---------------------------------------------------------------------------

/// SplitMix64: tiny, fast, and — unlike `rand::StdRng` — guaranteed to
/// produce the same sequence on every platform and dependency version.
#[derive(Debug, Clone)]
pub struct SeededRng(u64);

impl SeededRng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Independent stream derived from this seed and a label.
    pub fn derive(seed: u64, label: &str) -> Self {
        // FNV-1a over the label, mixed into the seed.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in label.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        let mut r = Self(seed ^ h);
        r.next_u64();
        r
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `lo..=hi`.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        assert!(lo <= hi);
        lo + self.next_u64() % (hi - lo + 1)
    }

    /// `true` with probability `bps / 10_000`.
    pub fn chance_bps(&mut self, bps: u32) -> bool {
        self.range(0, 9_999) < bps as u64
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.range(0, items.len() as u64 - 1) as usize]
    }
}

// ---------------------------------------------------------------------------
// Transcript
// ---------------------------------------------------------------------------

/// Ordered log of everything observable in a run.
#[derive(Default)]
pub struct Transcript {
    lines: Mutex<Vec<String>>,
}

impl Transcript {
    pub fn push(&self, line: impl Into<String>) {
        self.lines.lock().unwrap().push(line.into());
    }

    pub fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }

    pub fn digest(&self) -> String {
        let mut h = Sha256::new();
        for l in self.lines.lock().unwrap().iter() {
            h.update(l.as_bytes());
            h.update(b"\n");
        }
        hex::encode(h.finalize())
    }
}

/// JSON with object keys sorted at every level, so the transcript does not
/// depend on map ordering (e.g. JSONB's key order vs serde's).
pub fn canonical(v: &Value) -> String {
    fn sort(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let sorted: BTreeMap<_, _> = m.iter().map(|(k, v)| (k.clone(), sort(v))).collect();
                Value::Object(sorted.into_iter().collect())
            }
            Value::Array(a) => Value::Array(a.iter().map(sort).collect()),
            other => other.clone(),
        }
    }
    // serde_json::Map may preserve insertion order; build the string by hand.
    fn write(v: &Value, out: &mut String) {
        match v {
            Value::Object(m) => {
                let keys: BTreeSet<&String> = m.keys().collect();
                out.push('{');
                for (i, k) in keys.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(k).unwrap());
                    out.push(':');
                    write(&m[k], out);
                }
                out.push('}');
            }
            Value::Array(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(x, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut s = String::new();
    write(&sort(v), &mut s);
    s
}

// ---------------------------------------------------------------------------
// Mock liquidity provider
// ---------------------------------------------------------------------------

/// A scripted fault for the n-th `execute` call on a provider (1-based).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// Return `Unavailable`.
    Fail,
    /// Fill this many bps below the quoted output.
    Drift(u32),
}

#[derive(Debug, Clone)]
pub struct MockLpConfig {
    pub id: String,
    pub fee_bps: u32,
    /// Directed pairs and rates (`to` per `from`).
    pub rates: Vec<(String, String, Decimal)>,
    /// Random adverse fill drift, uniform in `0..=jitter_bps`.
    pub jitter_bps: u32,
    /// Probability (bps) a fill randomly fails.
    pub fail_bps: u32,
    /// Reject fills below `min_out` (a well-behaved venue).
    pub honours_min_out: bool,
}

impl MockLpConfig {
    pub fn new(id: &str, fee_bps: u32) -> Self {
        Self {
            id: id.into(),
            fee_bps,
            rates: Vec::new(),
            jitter_bps: 0,
            fail_bps: 0,
            honours_min_out: true,
        }
    }

    pub fn rate(mut self, from: &str, to: &str, rate: &str) -> Self {
        self.rates
            .push((from.into(), to.into(), Decimal::parse(rate).unwrap()));
        self
    }
}

pub struct MockLp {
    cfg: MockLpConfig,
    rates: BTreeMap<Pair, Decimal>,
    fee: FeeSchedule,
    policy: RoundingPolicy,
    rng: Mutex<SeededRng>,
    script: Mutex<BTreeMap<u64, Fault>>,
    executions: AtomicU64,
    /// Provider's net position per asset: + received, − paid out.
    inventory: Mutex<BTreeMap<String, Decimal>>,
    reversed: Mutex<BTreeSet<String>>,
    /// Fills by reference, with their (from, to) assets.
    fills: Mutex<BTreeMap<String, (LegFill, String, String)>>,
    compensation_fails: AtomicBool,
    transcript: Arc<Transcript>,
}

impl MockLp {
    pub fn new(cfg: MockLpConfig, seed: u64, transcript: Arc<Transcript>) -> Self {
        let policy = RoundingPolicy::STANDARD;
        let rates = cfg
            .rates
            .iter()
            .map(|(f, t, r)| {
                (
                    Pair::new(f.clone(), t.clone()),
                    r.rescale(RATE_SCALE, policy.mode(Purpose::Rate)),
                )
            })
            .collect();
        Self {
            rng: Mutex::new(SeededRng::derive(seed, &cfg.id)),
            fee: FeeSchedule::bps(cfg.fee_bps),
            rates,
            policy,
            cfg,
            script: Mutex::default(),
            executions: AtomicU64::new(0),
            inventory: Mutex::default(),
            reversed: Mutex::default(),
            fills: Mutex::default(),
            compensation_fails: AtomicBool::new(false),
            transcript,
        }
    }

    /// Inject a fault into the `nth` (1-based) execute call.
    pub fn script(&self, nth: u64, fault: Fault) {
        self.script.lock().unwrap().insert(nth, fault);
    }

    pub fn fail_compensations(&self, yes: bool) {
        self.compensation_fails.store(yes, Ordering::SeqCst);
    }

    pub fn execution_count(&self) -> u64 {
        self.executions.load(Ordering::SeqCst)
    }

    pub fn inventory(&self) -> BTreeMap<String, Decimal> {
        self.inventory.lock().unwrap().clone()
    }

    fn price(&self, from: &Asset, to: &Asset, amount_in: &Decimal, purpose: Purpose) -> Result<PairQuote, LiquidityError> {
        let rate = self
            .rates
            .get(&Pair::new(from.code.clone(), to.code.clone()))
            .ok_or_else(|| LiquidityError::UnsupportedPair(from.code.clone(), to.code.clone()))?;
        let (fee, net) = self.fee.split(amount_in, &self.policy);
        Ok(PairQuote {
            amount_in: amount_in.clone(),
            amount_out: convert(&net, rate, to, purpose, &self.policy),
            fee,
        })
    }

    fn adjust(&self, asset: &str, delta: &Decimal) {
        let mut inv = self.inventory.lock().unwrap();
        let cur = inv.remove(asset).unwrap_or_else(|| Decimal::zero(0));
        inv.insert(asset.to_string(), cur.add(delta));
    }
}

#[async_trait]
impl LiquidityProvider for MockLp {
    fn id(&self) -> &str {
        &self.cfg.id
    }

    async fn pairs(&self) -> Result<Vec<Pair>, LiquidityError> {
        Ok(self.rates.keys().cloned().collect())
    }

    async fn quote(&self, from: &Asset, to: &Asset, amount_in: &Decimal) -> Result<PairQuote, LiquidityError> {
        self.price(from, to, amount_in, Purpose::QuoteOutput)
    }

    async fn execute(&self, order: &LegOrder) -> Result<LegFill, LiquidityError> {
        let n = self.executions.fetch_add(1, Ordering::SeqCst) + 1;
        // Always draw both numbers so the stream position never depends on
        // which branch a call takes.
        let (fail_roll, jitter) = {
            let mut rng = self.rng.lock().unwrap();
            let fail_roll = rng.chance_bps(self.cfg.fail_bps);
            let jitter = rng.range(0, self.cfg.jitter_bps as u64) as u32;
            (fail_roll, jitter)
        };
        let scripted = self.script.lock().unwrap().remove(&n);
        let tag = format!(
            "lp {} execute#{n} {}#{} {}->{} in={} min_out={}",
            self.cfg.id,
            order.swap_id,
            order.leg_index,
            order.from.code,
            order.to.code,
            order.amount_in,
            order.min_out.as_ref().map_or("-".to_string(), |m| m.to_string()),
        );
        let failed = match scripted {
            Some(Fault::Fail) => true,
            Some(Fault::Drift(_)) => false,
            None => fail_roll,
        };
        if failed {
            self.transcript.push(format!("{tag} -> unavailable"));
            return Err(LiquidityError::Unavailable(format!("{} fill rejected", self.cfg.id)));
        }
        let drift = match scripted {
            Some(Fault::Drift(bps)) => bps,
            _ => jitter,
        };
        let q = self.price(&order.from, &order.to, &order.amount_in, Purpose::Fill)?;
        let out = q
            .amount_out
            .mul_bps(10_000 - drift, order.to.decimals, RoundingMode::Down);
        if let (true, Some(min)) = (self.cfg.honours_min_out, &order.min_out) {
            if &out < min {
                self.transcript
                    .push(format!("{tag} drift={drift}bps -> min_out_not_met out={out}"));
                return Err(LiquidityError::MinOutNotMet {
                    actual: out,
                    min: min.clone(),
                });
            }
        }
        let fill = LegFill {
            provider: self.cfg.id.clone(),
            reference: format!("{}:{}:{}", self.cfg.id, order.swap_id, order.leg_index),
            amount_in: order.amount_in.clone(),
            amount_out: out.clone(),
            fee: q.fee,
        };
        self.adjust(&order.from.code, &order.amount_in);
        self.adjust(&order.to.code, &Decimal::zero(0).sub(&out));
        self.fills.lock().unwrap().insert(
            fill.reference.clone(),
            (fill.clone(), order.from.code.clone(), order.to.code.clone()),
        );
        self.transcript
            .push(format!("{tag} drift={drift}bps -> filled out={out} ref={}", fill.reference));
        Ok(fill)
    }

    async fn compensate(&self, fill: &LegFill) -> Result<(), LiquidityError> {
        if self.compensation_fails.load(Ordering::SeqCst) {
            self.transcript
                .push(format!("lp {} compensate {} -> failed", self.cfg.id, fill.reference));
            return Err(LiquidityError::Unavailable("reversal rejected".into()));
        }
        let first = self.reversed.lock().unwrap().insert(fill.reference.clone());
        if first {
            let original = self.fills.lock().unwrap().get(&fill.reference).cloned();
            if let Some((f, from, to)) = original {
                self.adjust(&from, &Decimal::zero(0).sub(&f.amount_in));
                self.adjust(&to, &f.amount_out);
            }
        }
        self.transcript.push(format!(
            "lp {} compensate {} -> {}",
            self.cfg.id,
            fill.reference,
            if first { "reversed" } else { "already reversed" }
        ));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Mock price oracle
// ---------------------------------------------------------------------------

pub struct MockOracle {
    /// USD price per asset; reference rate = price(from) / price(to).
    prices: Mutex<BTreeMap<String, Decimal>>,
    down: AtomicBool,
    transcript: Arc<Transcript>,
}

impl MockOracle {
    pub fn new(prices: &[(&str, &str)], transcript: Arc<Transcript>) -> Self {
        Self {
            prices: Mutex::new(
                prices
                    .iter()
                    .map(|(a, p)| (a.to_string(), Decimal::parse(p).unwrap()))
                    .collect(),
            ),
            down: AtomicBool::new(false),
            transcript,
        }
    }

    pub fn set_price(&self, asset: &str, price: &str) {
        self.prices
            .lock()
            .unwrap()
            .insert(asset.into(), Decimal::parse(price).unwrap());
    }

    pub fn set_down(&self, down: bool) {
        self.down.store(down, Ordering::SeqCst);
    }

    pub fn rate(&self, from: &str, to: &str) -> Option<Decimal> {
        let p = self.prices.lock().unwrap();
        p.get(from)?
            .div(p.get(to)?, RATE_SCALE, RoundingMode::HalfEven)
            .ok()
    }
}

#[async_trait]
impl PriceOracle for MockOracle {
    fn id(&self) -> &str {
        "mock-oracle"
    }

    async fn reference_rate(&self, from: &Asset, to: &Asset) -> Result<Decimal, OracleError> {
        if self.down.load(Ordering::SeqCst) {
            self.transcript
                .push(format!("oracle {}->{} -> unavailable", from.code, to.code));
            return Err(OracleError::Unavailable("mock oracle down".into()));
        }
        let r = self
            .rate(&from.code, &to.code)
            .ok_or_else(|| OracleError::UnknownPair(from.code.clone(), to.code.clone()))?;
        self.transcript
            .push(format!("oracle {}->{} -> {r}", from.code, to.code));
        Ok(r)
    }
}

// ---------------------------------------------------------------------------
// Test database
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbKind {
    InMemory,
    /// Fresh Postgres container (needs Docker).
    Postgres,
}

pub struct TestDb {
    pub store: Arc<dyn SwapStore>,
    _container: Option<Box<dyn std::any::Any + Send + Sync>>,
}

impl TestDb {
    pub async fn start(kind: DbKind) -> Self {
        match kind {
            DbKind::InMemory => Self {
                store: Arc::new(InMemorySwapStore::new()),
                _container: None,
            },
            DbKind::Postgres => {
                use synapse_core::swap::pg_store::PgSwapStore;
                use testcontainers::runners::AsyncRunner;
                use testcontainers_modules::postgres::Postgres;
                let container = Postgres::default()
                    .start()
                    .await
                    .expect("start Postgres container (is Docker running?)");
                let port = container.get_host_port_ipv4(5432).await.unwrap();
                let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
                let pool = sqlx::PgPool::connect(&url).await.unwrap();
                let store = PgSwapStore::new(pool);
                store.ensure_schema().await.unwrap();
                Self {
                    store: Arc::new(store),
                    _container: Some(Box::new(container)),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

pub struct HarnessConfig {
    pub seed: u64,
    pub db: DbKind,
    pub assets: Vec<Asset>,
    pub lps: Vec<MockLpConfig>,
    /// USD prices for the oracle.
    pub oracle_prices: Vec<(&'static str, &'static str)>,
    pub router: RouterConfig,
    pub quote_ttl_secs: i64,
}

impl HarnessConfig {
    /// The standard market used by most scenarios:
    ///
    /// ```text
    ///   USDC ─lp-alpha─ XLM ─lp-beta─ EURC        (no direct USDC/EURC on alpha/beta)
    ///    │               └──lp-beta── NGN
    ///    ├─lp-gamma─ EURC (direct, but expensive)
    ///    ├─lp-gamma─ BTC (8 decimals)
    ///    └─lp-delta─ NGN
    /// ```
    pub fn standard(seed: u64) -> Self {
        Self {
            seed,
            db: DbKind::InMemory,
            assets: vec![
                Asset::stellar("USDC"),
                Asset::stellar("XLM"),
                Asset::stellar("EURC"),
                Asset::stellar("NGN"),
                Asset::new("BTC", 8),
            ],
            lps: vec![
                MockLpConfig::new("lp-alpha", 30)
                    .rate("USDC", "XLM", "9.98")
                    .rate("XLM", "USDC", "0.0998"),
                MockLpConfig::new("lp-beta", 20)
                    .rate("XLM", "EURC", "0.0924")
                    .rate("EURC", "XLM", "10.78")
                    .rate("XLM", "NGN", "153.5"),
                MockLpConfig::new("lp-gamma", 80)
                    .rate("USDC", "EURC", "0.92")
                    .rate("EURC", "USDC", "1.07")
                    .rate("USDC", "BTC", "0.0000166")
                    .rate("BTC", "USDC", "59900"),
                MockLpConfig::new("lp-delta", 50)
                    .rate("NGN", "USDC", "0.00064")
                    .rate("USDC", "NGN", "1530"),
            ],
            oracle_prices: vec![
                ("USDC", "1"),
                ("XLM", "0.1"),
                ("EURC", "1.08"),
                ("NGN", "0.00065"),
                ("BTC", "60000"),
            ],
            router: RouterConfig::default(),
            quote_ttl_secs: 30,
        }
    }

    pub fn with_db(mut self, db: DbKind) -> Self {
        self.db = db;
        self
    }

    pub fn map_lp(mut self, id: &str, f: impl FnOnce(MockLpConfig) -> MockLpConfig) -> Self {
        let i = self.lps.iter().position(|l| l.id == id).expect("unknown lp");
        let cfg = self.lps.remove(i);
        self.lps.insert(i, f(cfg));
        self
    }
}

pub fn start_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

pub struct SwapHarness {
    pub seed: u64,
    pub app: axum::Router,
    pub clock: Arc<ManualClock>,
    pub lps: BTreeMap<String, Arc<MockLp>>,
    pub oracle: Arc<MockOracle>,
    pub transcript: Arc<Transcript>,
    pub rng: SeededRng,
    _db: TestDb,
}

impl SwapHarness {
    pub async fn start(cfg: HarnessConfig) -> Self {
        let transcript = Arc::new(Transcript::default());
        // The db kind is deliberately not logged: the same seed must give the
        // same transcript on every store.
        transcript.push(format!("harness seed={:#x}", cfg.seed));
        let db = TestDb::start(cfg.db).await;
        let clock = Arc::new(ManualClock::new(start_time()));
        let oracle = Arc::new(MockOracle::new(&cfg.oracle_prices, transcript.clone()));
        let mut builder = SwapService::builder(AssetRegistry::new(cfg.assets))
            .router_config(cfg.router)
            .store(db.store.clone())
            .clock(clock.clone())
            .ids(Arc::new(SequentialIdGenerator::default()))
            .oracle(oracle.clone())
            .quote_ttl(Duration::seconds(cfg.quote_ttl_secs));
        let mut lps = BTreeMap::new();
        for lp_cfg in cfg.lps {
            let lp = Arc::new(MockLp::new(lp_cfg, cfg.seed, transcript.clone()));
            lps.insert(lp.id().to_string(), lp.clone());
            builder = builder.provider(lp);
        }
        let service = Arc::new(builder.build());
        Self {
            seed: cfg.seed,
            app: http::router(service),
            clock,
            lps,
            oracle,
            transcript,
            rng: SeededRng::derive(cfg.seed, "scenario"),
            _db: db,
        }
    }

    pub fn lp(&self, id: &str) -> &Arc<MockLp> {
        &self.lps[id]
    }

    pub fn advance(&self, secs: i64) {
        self.clock.advance(Duration::seconds(secs));
        self.transcript.push(format!("clock +{secs}s"));
    }

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .body(match body {
                Some(b) => Body::from(b.to_string()),
                None => Body::empty(),
            })
            .unwrap();
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status().as_u16();
        let bytes = hyper::body::to_bytes(resp.into_body()).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        self.transcript.push(format!(
            "{method} {path} {} -> {status} {}",
            body.map(canonical).unwrap_or_default(),
            canonical(&value)
        ));
        (status, value)
    }

    pub async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        self.call("POST", path, Some(&body)).await
    }

    pub async fn get(&self, path: &str) -> (u16, Value) {
        self.call("GET", path, None).await
    }

    pub async fn quote(&self, src: &str, dst: &str, amount: &str, protection: Option<Value>) -> (u16, Value) {
        let mut body = json!({ "source_asset": src, "destination_asset": dst, "amount_in": amount });
        if let Some(p) = protection {
            body["protection"] = p;
        }
        self.post("/swap/quotes", body).await
    }

    pub async fn execute(&self, quote_id: &str, protection: Option<Value>) -> (u16, Value) {
        let mut body = json!({ "quote_id": quote_id });
        if let Some(p) = protection {
            body["protection"] = p;
        }
        self.post("/swap/executions", body).await
    }

    pub async fn events(&self, swap_id: &str) -> Vec<Value> {
        let (status, v) = self.get(&format!("/swap/executions/{swap_id}/events")).await;
        assert_eq!(status, 200, "events for {swap_id}: {v}");
        v.as_array().cloned().unwrap_or_default()
    }

    pub fn digest(&self) -> String {
        self.transcript.digest()
    }

    /// Guard that prints how to reproduce this run if the test panics.
    pub fn guard(&self, test: &'static str) -> SeedGuard {
        SeedGuard {
            seed: self.seed,
            test,
            transcript: self.transcript.clone(),
        }
    }
}

pub struct SeedGuard {
    seed: u64,
    test: &'static str,
    transcript: Arc<Transcript>,
}

impl Drop for SeedGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let lines = self.transcript.lines();
            let tail = lines.len().saturating_sub(40);
            eprintln!("\n=== swap e2e failure: {} (seed {:#x}) ===", self.test, self.seed);
            eprintln!(
                "reproduce: SWAP_E2E_SEED={:#x} cargo test --test swap_e2e_harness_test {} -- --exact --nocapture",
                self.seed, self.test
            );
            eprintln!("--- last {} transcript lines ---", lines.len() - tail);
            for l in &lines[tail..] {
                eprintln!("{l}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Assertions over swap records
// ---------------------------------------------------------------------------

pub fn dec(v: &Value) -> Decimal {
    Decimal::parse(v.as_str().unwrap_or_else(|| panic!("expected decimal string, got {v}"))).unwrap()
}

/// Invariants every terminal swap record must satisfy.
pub fn assert_swap_invariants(quote: &Value, swap: &Value, events: &[Value]) {
    let status = swap["status"].as_str().unwrap();
    assert_eq!(swap["route"], quote["route"], "route must be carried from quote to execution");
    assert_eq!(swap["quote_id"], quote["quote_id"]);
    let legs = swap["legs"].as_array().unwrap();
    assert_eq!(legs.len(), quote["route"]["hops"].as_array().unwrap().len());

    // Event log: contiguous seq, starts executing, ends with the status.
    for (i, e) in events.iter().enumerate() {
        assert_eq!(e["seq"].as_u64().unwrap(), i as u64 + 1, "event seq gap");
    }
    assert_eq!(events.first().unwrap()["kind"], "swap.executing");
    assert_eq!(events.last().unwrap()["kind"], format!("swap.{status}"));

    match status {
        "settled" => {
            let out = dec(&swap["amount_out"]);
            if !swap["min_out"].is_null() {
                assert!(out >= dec(&swap["min_out"]), "settled below floor: {swap}");
            }
            assert!(legs.iter().all(|l| l["status"] == "filled"));
            // Each leg's actual input is the previous leg's actual output.
            assert_eq!(dec(&legs[0]["amount_in"]), dec(&swap["amount_in"]));
            for w in legs.windows(2) {
                assert_eq!(dec(&w[1]["amount_in"]), dec(&w[0]["amount_out"]));
            }
            assert_eq!(dec(&legs.last().unwrap()["amount_out"]), out);
            assert!(swap["failure"].is_null());
        }
        "compensated" => {
            assert!(swap["amount_out"].is_null());
            assert!(!swap["failure"].is_null());
            for l in legs {
                let s = l["status"].as_str().unwrap();
                assert!(
                    matches!(s, "compensated" | "failed" | "skipped"),
                    "leg left {s} in a compensated swap: {swap}"
                );
            }
        }
        other => panic!("swap not terminal: {other}: {swap}"),
    }
}

/// Every provider's net inventory must equal what settled swaps moved
/// through it — i.e. compensation fully unwound everything else.
pub fn assert_inventory_conserved(h: &SwapHarness, settled: &[Value]) {
    let mut expected: BTreeMap<String, BTreeMap<String, Decimal>> = BTreeMap::new();
    for s in settled {
        for l in s["legs"].as_array().unwrap() {
            let lp = expected.entry(l["provider"].as_str().unwrap().into()).or_default();
            let from = l["from"].as_str().unwrap().to_string();
            let to = l["to"].as_str().unwrap().to_string();
            let add = |m: &mut BTreeMap<String, Decimal>, k: String, d: Decimal| {
                let cur = m.remove(&k).unwrap_or_else(|| Decimal::zero(0));
                m.insert(k, cur.add(&d));
            };
            add(lp, from, dec(&l["amount_in"]));
            add(lp, to, Decimal::zero(0).sub(&dec(&l["amount_out"])));
        }
    }
    for (id, lp) in &h.lps {
        let actual = lp.inventory();
        let want = expected.remove(id).unwrap_or_default();
        let keys: BTreeSet<&String> = actual.keys().chain(want.keys()).collect();
        for k in keys {
            let a = actual.get(k).cloned().unwrap_or_else(|| Decimal::zero(0));
            let w = want.get(k).cloned().unwrap_or_else(|| Decimal::zero(0));
            assert_eq!(a, w, "{id} inventory of {k}: actual {a}, expected {w}");
        }
    }
}
