//! Chaos fault injection engine for database resilience testing.
//!
//! Provides deterministic TCP-level proxy fault injection, seeded PRNG for
//! failure reproducibility, connection drop simulation, latency spikes,
//! and pool exhaustion injection.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

/// Deterministic Pseudo-Random Number Generator (Xorshift64)
/// for 100% reproducible chaos runs given a seed.
#[derive(Debug, Clone)]
pub struct ChaosRng {
    state: u64,
}

impl ChaosRng {
    pub fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 0x853c49e6748fea9b } else { seed },
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    pub fn gen_bool(&mut self, probability: f64) -> bool {
        if probability <= 0.0 {
            return false;
        }
        if probability >= 1.0 {
            return true;
        }
        let val = (self.next_u64() % 10_000) as f64 / 10_000.0;
        val < probability
    }

    pub fn gen_range(&mut self, min: u64, max: u64) -> u64 {
        if min >= max {
            return min;
        }
        min + (self.next_u64() % (max - min + 1))
    }
}

/// Types of faults that can be injected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FaultKind {
    ConnectionDrop,
    LatencySpike { delay_ms: u64 },
    PoolExhaustion,
}

/// Configuration for chaos fault injection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChaosConfig {
    pub seed: u64,
    pub failure_rate: f64,
    pub drop_probability: f64,
    pub latency_probability: f64,
    pub exhaustion_probability: f64,
    pub min_latency_ms: u64,
    pub max_latency_ms: u64,
    pub enabled: bool,
}

impl Default for ChaosConfig {
    fn default() -> Self {
        Self {
            seed: 42,
            failure_rate: 0.3,
            drop_probability: 0.4,
            latency_probability: 0.4,
            exhaustion_probability: 0.2,
            min_latency_ms: 50,
            max_latency_ms: 300,
            enabled: true,
        }
    }
}

impl ChaosConfig {
    pub fn with_seed(seed: u64) -> Self {
        Self {
            seed,
            ..Default::default()
        }
    }

    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }
}

/// Statistics tracking chaos harness metrics.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ChaosStats {
    pub total_requests: u64,
    pub faults_injected: u64,
    pub connection_drops: u64,
    pub latency_spikes: u64,
    pub pool_exhaustions: u64,
}

/// Thread-safe fault injector state.
#[derive(Debug, Clone)]
pub struct FaultInjector {
    config: ChaosConfig,
    rng: Arc<Mutex<ChaosRng>>,
    stats: Arc<Mutex<ChaosStats>>,
}

impl FaultInjector {
    pub fn new(config: ChaosConfig) -> Self {
        let rng = ChaosRng::new(config.seed);
        Self {
            config,
            rng: Arc::new(Mutex::new(rng)),
            stats: Arc::new(Mutex::new(ChaosStats::default())),
        }
    }

    pub fn config(&self) -> &ChaosConfig {
        &self.config
    }

    pub fn stats(&self) -> ChaosStats {
        self.stats.lock().unwrap().clone()
    }

    pub fn reset_stats(&self) {
        let mut stats = self.stats.lock().unwrap();
        *stats = ChaosStats::default();
    }

    /// Evaluates whether to inject a fault, returning the injected fault kind if any.
    pub fn evaluate_fault(&self) -> Option<FaultKind> {
        if !self.config.enabled {
            return None;
        }

        let mut rng = self.rng.lock().unwrap();
        let mut stats = self.stats.lock().unwrap();
        stats.total_requests += 1;

        if !rng.gen_bool(self.config.failure_rate) {
            return None;
        }

        stats.faults_injected += 1;

        let roll = (rng.next_u64() % 100) as f64 / 100.0;
        let p_drop = self.config.drop_probability;
        let p_latency = self.config.latency_probability;

        if roll < p_drop {
            stats.connection_drops += 1;
            Some(FaultKind::ConnectionDrop)
        } else if roll < p_drop + p_latency {
            let delay_ms = rng.gen_range(self.config.min_latency_ms, self.config.max_latency_ms);
            stats.latency_spikes += 1;
            Some(FaultKind::LatencySpike { delay_ms })
        } else {
            stats.pool_exhaustions += 1;
            Some(FaultKind::PoolExhaustion)
        }
    }
}

/// TCP Chaos Proxy for database wire protocol fault injection.
pub struct ChaosProxy {
    listener_addr: SocketAddr,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    injector: FaultInjector,
}

impl ChaosProxy {
    /// Start a new Chaos Proxy relaying traffic to `target_addr`.
    pub async fn start(target_addr: SocketAddr, config: ChaosConfig) -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let listener_addr = listener.local_addr()?;
        let injector = FaultInjector::new(config);

        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let injector_clone = injector.clone();

        tokio::spawn(async move {
            let shutdown_notify = Arc::new(Notify::new());

            loop {
                tokio::select! {
                    res = listener.accept() => {
                        match res {
                            Ok((client_stream, _)) => {
                                let injector = injector_clone.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = Self::handle_connection(client_stream, target_addr, injector).await {
                                        tracing::debug!("Chaos proxy connection closed: {:?}", e);
                                    }
                                });
                            }
                            Err(e) => {
                                tracing::error!("Chaos proxy accept error: {:?}", e);
                                break;
                            }
                        }
                    }
                    _ = &mut shutdown_rx => {
                        tracing::info!("Shutting down Chaos Proxy on {}", listener_addr);
                        shutdown_notify.notify_waiters();
                        break;
                    }
                }
            }
        });

        Ok(Self {
            listener_addr,
            shutdown_tx: Some(shutdown_tx),
            injector,
        })
    }

    pub fn listener_addr(&self) -> SocketAddr {
        self.listener_addr
    }

    pub fn injector(&self) -> &FaultInjector {
        &self.injector
    }

    /// Construct postgres connection URL pointing to proxy.
    pub fn proxy_db_url(&self, original_url: &str) -> String {
        if let Ok(mut parsed) = url::Url::parse(original_url) {
            let _ = parsed.set_ip_host(self.listener_addr.ip());
            let _ = parsed.set_port(Some(self.listener_addr.port()));
            parsed.to_string()
        } else {
            format!("postgres://127.0.0.1:{}/synapse_test", self.listener_addr.port())
        }
    }

    async fn handle_connection(
        mut client_stream: TcpStream,
        target_addr: SocketAddr,
        injector: FaultInjector,
    ) -> anyhow::Result<()> {
        let fault = injector.evaluate_fault();

        match fault {
            Some(FaultKind::ConnectionDrop) => {
                tracing::info!("ChaosProxy: Injecting ConnectionDrop!");
                client_stream.shutdown().await?;
                return Ok(());
            }
            Some(FaultKind::LatencySpike { delay_ms }) => {
                tracing::info!("ChaosProxy: Injecting LatencySpike ({} ms)", delay_ms);
                tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
            }
            Some(FaultKind::PoolExhaustion) => {
                tracing::info!("ChaosProxy: Injecting PoolExhaustion stall (500 ms)");
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            }
            None => {}
        }

        let mut target_stream = match TcpStream::connect(target_addr).await {
            Ok(s) => s,
            Err(e) => {
                client_stream.shutdown().await?;
                return Err(e.into());
            }
        };

        let (mut cr, mut cw) = client_stream.split();
        let (mut tr, mut tw) = target_stream.split();

        let client_to_target = tokio::io::copy(&mut cr, &mut tw);
        let target_to_client = tokio::io::copy(&mut tr, &mut cw);

        tokio::select! {
            _ = client_to_target => {},
            _ = target_to_client => {},
        }

        Ok(())
    }

    pub fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}
