use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use tokio::sync::RwLock;
use vaultrs::auth::approle;
use vaultrs::client::{Client, VaultClient, VaultClientSettingsBuilder};
use vaultrs::kv2;

/// Grace period during which the previous secret remains valid after rotation.
const ROTATION_GRACE_PERIOD: Duration = Duration::from_secs(300);
/// How often to poll Vault for updated secrets. This is now a *fallback*
/// cadence, not the primary detection path: `start_refresh_task` also
/// subscribes to `ROTATION_CHANNEL` so a rotation detected by any one
/// instance's poll is pushed to every other instance immediately instead of
/// each instance waiting up to a full `REFRESH_INTERVAL` on its own clock.
/// See the coordination-gap issue this fixes for why the two independent
/// polling clocks alone could double the effective grace period fleet-wide.
const REFRESH_INTERVAL: Duration = Duration::from_secs(300);
/// Redis pub/sub channel used to fan out "a rotation was just detected" to
/// every instance. Reuses the same Redis infrastructure/pattern already
/// proven for cross-instance coordination in `circuit_breaker.rs`.
const ROTATION_CHANNEL: &str = "secrets:rotation";

/// A double-buffered secret: keeps current and previous value.
/// During the grace period both are accepted for signature validation.
#[derive(Clone, Debug)]
pub struct RotatingSecret {
    pub current: String,
    pub previous: Option<(String, Instant)>,
}

impl RotatingSecret {
    pub fn new(value: String) -> Self {
        Self {
            current: value,
            previous: None,
        }
    }

    /// Returns all currently-valid values: current first, then previous if still in grace period.
    pub fn valid_values(&self) -> Vec<&str> {
        let mut values = vec![self.current.as_str()];
        if let Some((prev, rotated_at)) = &self.previous {
            if rotated_at.elapsed() < ROTATION_GRACE_PERIOD {
                values.push(prev.as_str());
            }
        }
        values
    }

    /// Rotate to a new value, demoting current to previous.
    pub fn rotate(&mut self, new_value: String) {
        let old = std::mem::replace(&mut self.current, new_value);
        self.previous = Some((old, Instant::now()));
    }

    /// Checks `candidate` against this secret's valid values, distinguishing
    /// which one matched. `Some(true)` = matched `current`; `Some(false)` =
    /// matched `previous` within the grace period; `None` = matched neither.
    /// Callers use the `Some(false)` case to record the
    /// `secrets_previous_value_verified_total` metric — a caller still
    /// presenting the old secret well after a rotation is exactly the signal
    /// that motivates bounding the fleet-wide propagation window.
    pub fn verify(&self, candidate: &str) -> Option<bool> {
        if candidate == self.current {
            return Some(true);
        }
        if let Some((prev, rotated_at)) = &self.previous {
            if rotated_at.elapsed() < ROTATION_GRACE_PERIOD && candidate == prev {
                return Some(false);
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Runtime Vault-outage fallback (#1336) — see docs/vault-runtime-degradation.md
// ---------------------------------------------------------------------------

/// Default hard maximum age of a last-known-good secret served while Vault
/// is unreachable, measured from its last successful Vault read.
/// Conservatively short: three normal refresh intervals.
pub const DEFAULT_FALLBACK_MAX_AGE: Duration = Duration::from_secs(900);
/// Lower clamp on the configured max age: it must exceed a normal refresh
/// gap, or healthy operation would expire secrets between polls.
pub const MIN_FALLBACK_MAX_AGE: Duration = Duration::from_secs(360);
/// Upper clamp: configuration can shorten the window but never make it
/// effectively unbounded.
pub const MAX_FALLBACK_MAX_AGE: Duration = Duration::from_secs(3600);
/// Refresh cadence while Vault is failing, so recovery is picked up long
/// before the fallback window closes.
pub const DEFAULT_OUTAGE_RETRY_INTERVAL: Duration = Duration::from_secs(30);

/// How long-lived a Vault-managed secret is by design, which decides
/// whether a stale copy may ever be served.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretClass {
    /// Long-lived, periodically rotated; serving it up to the bounded
    /// fallback age during a Vault outage is an explicitly approved
    /// tradeoff.
    LongLivedApproved,
    /// Short-lived by design (dynamic credentials, leases). Never served
    /// past its own refresh — a Vault outage makes it unavailable.
    ShortLived,
    /// Single-use by design (wrapped tokens, one-time secret IDs). Never
    /// cached at all.
    SingleUse,
}

impl SecretClass {
    pub fn fallback_allowed(self) -> bool {
        matches!(self, SecretClass::LongLivedApproved)
    }
}

/// The Vault-managed secrets `SecretsStore` holds, with their reviewed
/// class. Database credentials are deliberately *not* here: they are held
/// by the connection pool, not cached (issue 58).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretKind {
    AnchorWebhookSecret,
    AdminApiKey,
}

impl SecretKind {
    pub const ALL: [SecretKind; 2] = [SecretKind::AnchorWebhookSecret, SecretKind::AdminApiKey];

    pub fn name(self) -> &'static str {
        match self {
            SecretKind::AnchorWebhookSecret => "anchor_webhook_secret",
            SecretKind::AdminApiKey => "admin_api_key",
        }
    }

    /// Reviewed classification. Both are long-lived and rotated with a
    /// grace period already (see `RotatingSecret`), so bounded staleness is
    /// the same order of risk the grace period already accepts.
    pub fn class(self) -> SecretClass {
        match self {
            SecretKind::AnchorWebhookSecret => SecretClass::LongLivedApproved,
            SecretKind::AdminApiKey => SecretClass::LongLivedApproved,
        }
    }
}

/// Fallback behaviour configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VaultFallbackConfig {
    pub max_age: Duration,
    pub outage_retry_interval: Duration,
}

impl Default for VaultFallbackConfig {
    fn default() -> Self {
        Self {
            max_age: DEFAULT_FALLBACK_MAX_AGE,
            outage_retry_interval: DEFAULT_OUTAGE_RETRY_INTERVAL,
        }
    }
}

impl VaultFallbackConfig {
    /// `VAULT_SECRET_FALLBACK_MAX_AGE_SECS` (clamped to
    /// [`MIN_FALLBACK_MAX_AGE`]..=[`MAX_FALLBACK_MAX_AGE`]) and
    /// `VAULT_OUTAGE_RETRY_SECS` (clamped to 5..=300).
    pub fn from_env() -> Self {
        let secs = |var: &str| env::var(var).ok().and_then(|v| v.parse::<u64>().ok());
        Self::clamped(
            secs("VAULT_SECRET_FALLBACK_MAX_AGE_SECS").map(Duration::from_secs),
            secs("VAULT_OUTAGE_RETRY_SECS").map(Duration::from_secs),
        )
    }

    pub fn clamped(max_age: Option<Duration>, retry: Option<Duration>) -> Self {
        Self {
            max_age: max_age
                .unwrap_or(DEFAULT_FALLBACK_MAX_AGE)
                .clamp(MIN_FALLBACK_MAX_AGE, MAX_FALLBACK_MAX_AGE),
            outage_retry_interval: retry
                .unwrap_or(DEFAULT_OUTAGE_RETRY_INTERVAL)
                .clamp(Duration::from_secs(5), REFRESH_INTERVAL),
        }
    }
}

/// Whether a cached secret may be used right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretUsability {
    /// Refreshed from Vault within the normal cadence.
    Fresh,
    /// Vault is unreachable; serving the last-known-good value.
    CachedFallback { age: Duration, remaining: Duration },
    /// Past the hard maximum age (or not approved for fallback): refuse.
    Expired { age: Duration },
}

#[derive(Debug)]
struct Freshness {
    last_success: Instant,
    failing_since: Option<Instant>,
    consecutive_failures: u32,
    expiry_reported: bool,
}

impl Freshness {
    fn new(now: Instant) -> Self {
        Self {
            last_success: now,
            failing_since: None,
            consecutive_failures: 0,
            expiry_reported: false,
        }
    }
}

/// Pure decision: how usable is a secret last refreshed at `f.last_success`?
///
/// A secret is `Fresh` while refreshes succeed and it is within two normal
/// refresh intervals. Past that — whether because Vault is failing or the
/// refresh loop itself stopped — an approved secret is a `CachedFallback`
/// until `max_age`, then `Expired`; a non-approved one is `Expired` at once.
fn usability(
    class: SecretClass,
    f: &Freshness,
    max_age: Duration,
    now: Instant,
) -> SecretUsability {
    let age = now.saturating_duration_since(f.last_success);
    if f.failing_since.is_none() && age <= REFRESH_INTERVAL * 2 {
        return SecretUsability::Fresh;
    }
    if !class.fallback_allowed() || age > max_age {
        return SecretUsability::Expired { age };
    }
    SecretUsability::CachedFallback {
        age,
        remaining: max_age - age,
    }
}

/// Snapshot of one secret's Vault freshness, for `/ready`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SecretStatusReport {
    pub secret: SecretKind,
    pub class: SecretClass,
    /// `fresh` | `cached_fallback` | `expired`
    pub status: &'static str,
    pub age_secs: u64,
    pub fallback_remaining_secs: Option<u64>,
}

/// Aggregate Vault state, for `/ready` and gauges.
#[derive(Debug, Clone, serde::Serialize)]
pub struct VaultStatus {
    /// `ok` | `degraded_cached_fallback` | `expired`
    pub status: &'static str,
    pub unreachable_for_secs: Option<u64>,
    /// Smallest remaining fallback window across secrets on fallback.
    pub fallback_remaining_secs: Option<u64>,
    pub max_fallback_age_secs: u64,
    pub secrets: Vec<SecretStatusReport>,
}

/// Thread-safe store of rotating secrets shared across the application.
///
/// Also tracks, per secret, when it was last successfully read from Vault,
/// and enforces the bounded runtime-outage fallback on every verification
/// (see [`SecretUsability`]).
#[derive(Clone)]
pub struct SecretsStore {
    pub anchor_webhook_secret: Arc<RwLock<RotatingSecret>>,
    pub admin_api_key: Arc<RwLock<RotatingSecret>>,
    freshness: Arc<std::sync::Mutex<HashMap<SecretKind, Freshness>>>,
    fallback: VaultFallbackConfig,
}

impl SecretsStore {
    pub fn new(anchor_webhook_secret: String, admin_api_key: String) -> Self {
        Self::with_fallback_config(
            anchor_webhook_secret,
            admin_api_key,
            VaultFallbackConfig::from_env(),
        )
    }

    pub fn with_fallback_config(
        anchor_webhook_secret: String,
        admin_api_key: String,
        fallback: VaultFallbackConfig,
    ) -> Self {
        let now = Instant::now();
        Self {
            anchor_webhook_secret: Arc::new(RwLock::new(RotatingSecret::new(
                anchor_webhook_secret,
            ))),
            admin_api_key: Arc::new(RwLock::new(RotatingSecret::new(admin_api_key))),
            freshness: Arc::new(std::sync::Mutex::new(
                SecretKind::ALL
                    .iter()
                    .map(|&k| (k, Freshness::new(now)))
                    .collect(),
            )),
            fallback,
        }
    }

    pub fn fallback_config(&self) -> VaultFallbackConfig {
        self.fallback
    }

    fn secret(&self, kind: SecretKind) -> &Arc<RwLock<RotatingSecret>> {
        match kind {
            SecretKind::AnchorWebhookSecret => &self.anchor_webhook_secret,
            SecretKind::AdminApiKey => &self.admin_api_key,
        }
    }

    fn with_freshness<R>(&self, kind: SecretKind, f: impl FnOnce(&mut Freshness) -> R) -> R {
        let mut map = self.freshness.lock().unwrap_or_else(|p| p.into_inner());
        let entry = map
            .entry(kind)
            .or_insert_with(|| Freshness::new(Instant::now()));
        f(entry)
    }

    /// Current usability of `kind`'s cached value.
    pub fn usability_at(&self, kind: SecretKind, now: Instant) -> SecretUsability {
        self.with_freshness(kind, |f| {
            usability(kind.class(), f, self.fallback.max_age, now)
        })
    }

    /// Records a successful Vault read of `kind`.
    pub fn record_refresh_success(&self, kind: SecretKind, now: Instant) {
        self.with_freshness(kind, |f| {
            if let Some(since) = f.failing_since {
                tracing::info!(
                    secret = kind.name(),
                    outage_secs = now.saturating_duration_since(since).as_secs(),
                    "vault_fallback: Vault reachable again; secret refreshed, fallback cleared"
                );
            }
            *f = Freshness::new(now);
        });
    }

    /// Records a failed Vault read of `kind`, logging loudly on the
    /// transition into fallback and on every subsequent failure.
    pub fn record_refresh_failure(
        &self,
        kind: SecretKind,
        now: Instant,
        error: &dyn std::fmt::Display,
    ) {
        crate::metrics::vault_refresh_failures_total()
            .add(1, &[opentelemetry::KeyValue::new("secret", kind.name())]);
        let max_age = self.fallback.max_age;
        self.with_freshness(kind, |f| {
            let first = f.failing_since.is_none();
            f.failing_since.get_or_insert(now);
            f.consecutive_failures += 1;
            match usability(kind.class(), f, max_age, now) {
                SecretUsability::CachedFallback { age, remaining } => {
                    if first {
                        tracing::error!(
                            degraded_mode = true,
                            dependency = "vault",
                            secret = kind.name(),
                            age_secs = age.as_secs(),
                            fallback_remaining_secs = remaining.as_secs(),
                            error = %error,
                            "vault_fallback: Vault unreachable — serving last-known-good secret \
                             until the hard max age, then refusing it"
                        );
                    } else {
                        tracing::warn!(
                            degraded_mode = true,
                            dependency = "vault",
                            secret = kind.name(),
                            consecutive_failures = f.consecutive_failures,
                            age_secs = age.as_secs(),
                            fallback_remaining_secs = remaining.as_secs(),
                            error = %error,
                            "vault_fallback: Vault still unreachable; serving cached secret"
                        );
                    }
                }
                SecretUsability::Expired { age } => {
                    if !f.expiry_reported {
                        f.expiry_reported = true;
                        tracing::error!(
                            degraded_mode = true,
                            dependency = "vault",
                            secret = kind.name(),
                            age_secs = age.as_secs(),
                            fallback_allowed = kind.class().fallback_allowed(),
                            error = %error,
                            "vault_fallback: cached secret past its maximum fallback age — \
                             refusing to use it until Vault is reachable"
                        );
                    }
                }
                SecretUsability::Fresh => {}
            }
        });
    }

    /// Gate every use of a cached secret goes through. Returns `false` (and
    /// counts a rejection) when the secret must not be used.
    fn admit(&self, kind: SecretKind, now: Instant) -> bool {
        let label = [opentelemetry::KeyValue::new("secret", kind.name())];
        match self.usability_at(kind, now) {
            SecretUsability::Fresh => true,
            SecretUsability::CachedFallback { .. } => {
                crate::metrics::secrets_cached_fallback_used_total().add(1, &label);
                true
            }
            SecretUsability::Expired { age } => {
                crate::metrics::secrets_stale_rejected_total().add(1, &label);
                tracing::warn!(
                    secret = kind.name(),
                    age_secs = age.as_secs(),
                    "vault_fallback: refusing verification against an expired cached secret"
                );
                false
            }
        }
    }

    /// Returns all valid anchor webhook secret values (current + grace-period
    /// previous) — or none once the cached value has expired during a Vault
    /// outage, which makes signature verification fail closed.
    pub async fn valid_webhook_secrets(&self) -> Vec<String> {
        self.valid_values(SecretKind::AnchorWebhookSecret).await
    }

    /// Returns all valid admin API key values (current + grace-period
    /// previous), or none once expired (see `valid_webhook_secrets`).
    pub async fn valid_admin_keys(&self) -> Vec<String> {
        self.valid_values(SecretKind::AdminApiKey).await
    }

    async fn valid_values(&self, kind: SecretKind) -> Vec<String> {
        self.valid_values_at(kind, Instant::now()).await
    }

    async fn valid_values_at(&self, kind: SecretKind, now: Instant) -> Vec<String> {
        if !self.admit(kind, now) {
            return Vec::new();
        }
        self.secret(kind)
            .read()
            .await
            .valid_values()
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    /// Verifies `candidate` against the admin API key, recording
    /// `secrets_previous_value_verified_total` when it only matches the
    /// grace-period previous value rather than current.
    pub async fn verify_admin_key(&self, candidate: &str) -> bool {
        self.verify_and_record(SecretKind::AdminApiKey, candidate, Instant::now())
            .await
    }

    /// Verifies `candidate` against the anchor webhook secret, recording
    /// `secrets_previous_value_verified_total` when it only matches the
    /// grace-period previous value rather than current.
    pub async fn verify_webhook_secret(&self, candidate: &str) -> bool {
        self.verify_and_record(SecretKind::AnchorWebhookSecret, candidate, Instant::now())
            .await
    }

    async fn verify_and_record(&self, kind: SecretKind, candidate: &str, now: Instant) -> bool {
        if !self.admit(kind, now) {
            return false;
        }
        match self.secret(kind).read().await.verify(candidate) {
            Some(true) => true,
            Some(false) => {
                crate::metrics::secrets_previous_value_verified_total()
                    .add(1, &[opentelemetry::KeyValue::new("secret", kind.name())]);
                true
            }
            None => false,
        }
    }

    /// Aggregate Vault freshness for `/ready`.
    pub fn vault_status_at(&self, now: Instant) -> VaultStatus {
        let mut secrets = Vec::new();
        let mut unreachable_since: Option<Instant> = None;
        for kind in SecretKind::ALL {
            let (u, failing_since) = self.with_freshness(kind, |f| {
                (
                    usability(kind.class(), f, self.fallback.max_age, now),
                    f.failing_since,
                )
            });
            if let Some(since) = failing_since {
                unreachable_since = Some(unreachable_since.map_or(since, |u| u.min(since)));
            }
            let (status, age, remaining) = match u {
                SecretUsability::Fresh => (
                    "fresh",
                    self.with_freshness(kind, |f| now.saturating_duration_since(f.last_success)),
                    None,
                ),
                SecretUsability::CachedFallback { age, remaining } => {
                    ("cached_fallback", age, Some(remaining.as_secs()))
                }
                SecretUsability::Expired { age } => ("expired", age, None),
            };
            secrets.push(SecretStatusReport {
                secret: kind,
                class: kind.class(),
                status,
                age_secs: age.as_secs(),
                fallback_remaining_secs: remaining,
            });
        }
        let status = if secrets.iter().any(|s| s.status == "expired") {
            "expired"
        } else if secrets.iter().any(|s| s.status == "cached_fallback") {
            "degraded_cached_fallback"
        } else {
            "ok"
        };
        VaultStatus {
            status,
            unreachable_for_secs: unreachable_since
                .map(|s| now.saturating_duration_since(s).as_secs()),
            fallback_remaining_secs: secrets
                .iter()
                .filter_map(|s| s.fallback_remaining_secs)
                .min(),
            max_fallback_age_secs: self.fallback.max_age.as_secs(),
            secrets,
        }
    }

    pub fn vault_status(&self) -> VaultStatus {
        self.vault_status_at(Instant::now())
    }

    /// Registers `vault_secret_staleness_seconds{secret}` and
    /// `vault_fallback_active` gauges over this store. Keep the returned
    /// handles alive for the process lifetime.
    pub fn register_vault_gauges(
        &self,
    ) -> (
        opentelemetry::metrics::ObservableGauge<u64>,
        opentelemetry::metrics::ObservableGauge<u64>,
    ) {
        let meter = opentelemetry::global::meter("synapse-core");
        let store = self.clone();
        let staleness = meter
            .u64_observable_gauge("vault_secret_staleness_seconds")
            .with_description("Seconds since each secret was last read successfully from Vault")
            .with_callback(move |observer| {
                for report in store.vault_status().secrets {
                    observer.observe(
                        report.age_secs,
                        &[opentelemetry::KeyValue::new("secret", report.secret.name())],
                    );
                }
            })
            .init();
        let store = self.clone();
        let active = meter
            .u64_observable_gauge("vault_fallback_active")
            .with_description(
                "1 while any secret is served from the Vault fallback cache or has expired",
            )
            .with_callback(move |observer| {
                let degraded = store.vault_status().status != "ok";
                observer.observe(u64::from(degraded), &[]);
            })
            .init();
        (staleness, active)
    }
}

/// Where fresh secret values come from — Vault in production, a fake in
/// tests.
#[async_trait::async_trait]
pub trait SecretSource: Send + Sync {
    async fn fetch(&self, kind: SecretKind) -> Result<String>;
}

/// Outcome of one refresh pass.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RefreshReport {
    pub rotated: Vec<SecretKind>,
    pub failed: Vec<SecretKind>,
}

/// One refresh pass over every secret: rotates changed values, and records
/// success/failure so the bounded fallback is enforced.
pub async fn refresh_once(
    source: &dyn SecretSource,
    store: &SecretsStore,
    lag_ms: f64,
    via_pubsub: bool,
) -> RefreshReport {
    let mut report = RefreshReport::default();
    for kind in SecretKind::ALL {
        match source.fetch(kind).await {
            Ok(new_value) => {
                store.record_refresh_success(kind, Instant::now());
                let mut lock = store.secret(kind).write().await;
                if lock.current != new_value {
                    lock.rotate(new_value);
                    report.rotated.push(kind);
                    crate::metrics::secrets_rotation_detection_lag_ms().record(
                        lag_ms,
                        &[
                            opentelemetry::KeyValue::new("secret", kind.name()),
                            opentelemetry::KeyValue::new("via_pubsub", via_pubsub),
                        ],
                    );
                    tracing::info!(
                        "secrets_rotation: {} rotated; previous value valid for {}s grace period",
                        kind.name(),
                        ROTATION_GRACE_PERIOD.as_secs()
                    );
                }
            }
            Err(e) => {
                store.record_refresh_failure(kind, Instant::now(), &e);
                report.failed.push(kind);
            }
        }
    }
    report
}

/// Scorecard classification of a Vault read error (#1334): an API error
/// status means Vault answered; a REST client error means no response.
fn classify_vault_error(e: &anyhow::Error) -> crate::services::dependency_scorecard::CallOutcome {
    use crate::services::dependency_scorecard::CallOutcome;
    match e.downcast_ref::<vaultrs::error::ClientError>() {
        Some(vaultrs::error::ClientError::APIError { code, .. }) => {
            crate::services::dependency_scorecard::classify_http_status(*code)
        }
        Some(vaultrs::error::ClientError::RestClientError { .. }) => CallOutcome::TransportFailure,
        Some(_) => CallOutcome::DependencyFault,
        None => CallOutcome::TransportFailure,
    }
}

#[async_trait::async_trait]
impl SecretSource for SecretsManager {
    async fn fetch(&self, kind: SecretKind) -> Result<String> {
        use crate::services::dependency_scorecard::{self as scorecard, CallOutcome, Dependency};
        let started = std::time::Instant::now();
        let result = match kind {
            SecretKind::AnchorWebhookSecret => self.get_anchor_secret().await,
            SecretKind::AdminApiKey => self.get_admin_api_key().await,
        };
        let outcome = match &result {
            Ok(_) => CallOutcome::Success,
            Err(e) => classify_vault_error(e),
        };
        scorecard::record_call(Dependency::Vault, outcome, started.elapsed());
        result
    }
}

pub struct SecretsManager {
    client: VaultClient,
    kv_mount: String,
    vault_addr: String,
    client_token: String,
}

#[derive(Debug, serde::Deserialize)]
struct DatabaseLeaseResponse {
    lease_id: String,
    lease_duration: u64,
    data: DatabaseLeaseData,
}

#[derive(Debug, serde::Deserialize)]
struct DatabaseLeaseData {
    username: String,
    password: String,
}

#[derive(Debug, serde::Deserialize)]
struct LeaseRenewalResponse {
    lease_duration: u64,
}

impl SecretsManager {
    pub async fn new() -> Result<Self> {
        let vault_addr =
            env::var("VAULT_ADDR").unwrap_or_else(|_| "http://127.0.0.1:8200".to_string());
        let role_id = env::var("VAULT_ROLE_ID").context("VAULT_ROLE_ID is required")?;
        let secret_id = env::var("VAULT_SECRET_ID").context("VAULT_SECRET_ID is required")?;
        let auth_mount =
            env::var("VAULT_AUTH_MOUNT").unwrap_or_else(|_| "auth/approle".to_string());
        let kv_mount = env::var("VAULT_KV_MOUNT").unwrap_or_else(|_| "secret".to_string());

        let mut client = VaultClient::new(
            VaultClientSettingsBuilder::default()
                .address(&vault_addr)
                .build()
                .context("failed to build Vault client settings")?,
        )
        .context("failed to create Vault client")?;

        let auth = approle::login(&client, &auth_mount, &role_id, &secret_id)
            .await
            .context("failed to authenticate to Vault with AppRole")?;
        client.set_token(&auth.client_token);

        Ok(Self {
            client,
            kv_mount,
            vault_addr,
            client_token: auth.client_token,
        })
    }

    pub async fn get_db_password(&self) -> Result<String> {
        let secret: HashMap<String, String> = kv2::read(&self.client, &self.kv_mount, "database")
            .await
            .context("failed to read secret/database from Vault")?;

        secret
            .get("password")
            .cloned()
            .context("password key not found in Vault secret/database")
    }

    pub async fn get_anchor_secret(&self) -> Result<String> {
        let secret: HashMap<String, String> = kv2::read(&self.client, &self.kv_mount, "anchor")
            .await
            .context("failed to read secret/anchor from Vault")?;

        secret
            .get("secret")
            .cloned()
            .context("secret key not found in Vault secret/anchor")
    }

    pub async fn get_admin_api_key(&self) -> Result<String> {
        let secret: HashMap<String, String> = kv2::read(&self.client, &self.kv_mount, "admin")
            .await
            .context("failed to read secret/admin from Vault")?;

        secret
            .get("api_key")
            .cloned()
            .context("api_key not found in Vault secret/admin")
    }

    async fn issue_database_lease(&self, role: &str) -> Result<DatabaseLeaseResponse> {
        let response = reqwest::Client::new()
            .get(format!("{}/v1/database/creds/{role}", self.vault_addr.trim_end_matches('/')))
            .header("X-Vault-Token", &self.client_token)
            .send()
            .await
            .context("failed to request dynamic database credentials from Vault")?;
        response
            .error_for_status()
            .context("Vault rejected dynamic database credential request")?
            .json()
            .await
            .context("invalid Vault database credential response")
    }

    async fn renew_database_lease(&self, lease_id: &str) -> Result<u64> {
        let response: LeaseRenewalResponse = reqwest::Client::new()
            .put(format!("{}/v1/sys/leases/renew", self.vault_addr.trim_end_matches('/')))
            .header("X-Vault-Token", &self.client_token)
            .json(&serde_json::json!({ "lease_id": lease_id }))
            .send()
            .await
            .context("failed to renew Vault database lease")?
            .error_for_status()
            .context("Vault rejected database lease renewal")?
            .json()
            .await
            .context("invalid Vault lease renewal response")?;
        Ok(response.lease_duration)
    }

    /// Keep Vault database credentials alive and replace the active pool before
    /// the lease expires. The pool manager atomically swaps handles and drains
    /// the old pool, so requests already in progress are never interrupted.
    pub fn start_database_rotation_task(
        &self,
        pool_manager: crate::db::pool_manager::PoolManager,
        role: String,
        database_url_template: String,
    ) {
        let vault_addr = self.vault_addr.clone();
        let client_token = self.client_token.clone();
        tokio::spawn(async move {
            let http = reqwest::Client::new();
            let mut lease: Option<DatabaseLeaseResponse> = None;
            loop {
                let next = match &lease {
                    Some(current) => {
                        let renewal = http
                            .put(format!("{}/v1/sys/leases/renew", vault_addr.trim_end_matches('/')))
                            .header("X-Vault-Token", &client_token)
                            .json(&serde_json::json!({ "lease_id": current.lease_id }))
                            .send()
                            .await;
                        match renewal {
                            Ok(response) if response.status().is_success() => {
                                response.json::<LeaseRenewalResponse>().await.ok().map(|renewed| {
                                    tokio::time::sleep(Duration::from_secs((renewed.lease_duration / 2).max(5)));
                                    renewed.lease_duration
                                })
                            }
                            _ => None,
                        }
                    }
                    None => None,
                };

                if next.is_none() {
                    match http
                        .get(format!("{}/v1/database/creds/{role}", vault_addr.trim_end_matches('/')))
                        .header("X-Vault-Token", &client_token)
                        .send()
                        .await
                    {
                        Ok(response) if response.status().is_success() => {
                            match response.json::<DatabaseLeaseResponse>().await {
                                Ok(new_lease) => {
                                    let database_url = database_url_template
                                        .replace("{username}", &new_lease.data.username)
                                        .replace("{password}", &new_lease.data.password);
                                    if let Err(error) = pool_manager.rotate_primary(&database_url).await {
                                        tracing::error!("Vault credential rotation pool swap failed: {error}");
                                    } else {
                                        tracing::info!(lease_id = %new_lease.lease_id, "Vault database credentials rotated");
                                    }
                                    let delay = Duration::from_secs((new_lease.lease_duration / 2).max(5));
                                    lease = Some(new_lease);
                                    tokio::time::sleep(delay).await;
                                    continue;
                                }
                                Err(error) => tracing::error!("Invalid Vault database lease: {error}"),
                            }
                        }
                        Ok(response) => tracing::error!(status = %response.status(), "Vault database credential request failed"),
                        Err(error) => tracing::error!("Vault database credential request failed: {error}"),
                    }
                    tokio::time::sleep(Duration::from_secs(30)).await;
                } else {
                    tokio::time::sleep(Duration::from_secs(next.unwrap().max(5) / 2)).await;
                }
            }
        });
    }

    /// Spawn the background tasks that keep secrets fresh and coordinated
    /// across the fleet:
    ///
    /// - A poll loop (unchanged cadence, `REFRESH_INTERVAL`) that remains the
    ///   sole source of truth for *whether* a rotation happened, and the
    ///   fallback detection path if pub/sub is unavailable.
    /// - A dedicated pub/sub listener (reusing the Redis infrastructure
    ///   already proven for `circuit_breaker.rs`'s cross-instance
    ///   coordination) that wakes the poll loop immediately when *any*
    ///   instance's poll detects and announces a rotation, instead of this
    ///   instance waiting up to its own `REFRESH_INTERVAL`.
    ///
    /// Without this, two instances' independently-clocked polls could leave
    /// the old secret valid fleet-wide for up to `REFRESH_INTERVAL +
    /// ROTATION_GRACE_PERIOD` — double what either constant suggests alone.
    pub fn start_refresh_task(self, store: SecretsStore, redis_url: String) {
        let (notify_tx, mut notify_rx) = tokio::sync::mpsc::unbounded_channel::<i64>();

        // Dedicated listener: owns its own pub/sub connection, reconnects
        // with backoff if it drops, and forwards the publish timestamp of
        // any rotation notification. Kept separate from the refresh loop
        // below so a Redis outage degrades this to poll-only (the refresh
        // loop's `tokio::select!` simply never receives from `notify_rx`)
        // rather than blocking secret refresh entirely.
        {
            let redis_url = redis_url.clone();
            tokio::spawn(async move {
                loop {
                    match Self::connect_rotation_pubsub(&redis_url).await {
                        Some(mut pubsub) => {
                            let mut stream = pubsub.on_message();
                            while let Some(msg) = stream.next().await {
                                let published_at_ms = msg
                                    .get_payload::<String>()
                                    .ok()
                                    .and_then(|p| p.parse::<i64>().ok())
                                    .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
                                let _ = notify_tx.send(published_at_ms);
                            }
                            // Stream ended: connection dropped; fall through to reconnect.
                        }
                        None => {
                            crate::metrics::secrets_rotation_pubsub_unavailable_total().add(1, &[]);
                            crate::cache::degradation::record_redis_degraded(
                                crate::cache::degradation::RedisComponent::SecretsRotationPubSub,
                                crate::cache::degradation::DegradedFallback::PollOnly,
                                &"rotation pub/sub unavailable",
                            );
                        }
                    }
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            });
        }

        let publish_client = redis::Client::open(redis_url.as_str()).ok();
        if publish_client.is_none() {
            tracing::warn!(
                "secrets_rotation: could not build Redis client for rotation \
                 announcements ({redis_url}); this instance will still detect \
                 its own rotations via polling but cannot notify others"
            );
        }

        let fallback = store.fallback_config();
        tokio::spawn(async move {
            let mut next_wait = REFRESH_INTERVAL;
            loop {
                let published_at_ms = tokio::select! {
                    _ = tokio::time::sleep(next_wait) => None,
                    Some(ts) = notify_rx.recv() => Some(ts),
                };
                let via_pubsub = published_at_ms.is_some();
                let lag_ms = published_at_ms
                    .map(|ts| (chrono::Utc::now().timestamp_millis() - ts).max(0) as f64)
                    .unwrap_or(0.0);

                tracing::info!(
                    via_pubsub,
                    "secrets_rotation: refreshing secrets from Vault"
                );

                let report = refresh_once(&self, &store, lag_ms, via_pubsub).await;

                // While Vault is failing, retry on the short outage cadence
                // so recovery lands well inside the fallback window.
                next_wait = if report.failed.is_empty() {
                    REFRESH_INTERVAL
                } else {
                    fallback.outage_retry_interval
                };

                // Announce to the rest of the fleet so their grace-period
                // clocks start now instead of on their own next poll tick.
                // Redundant if another instance already announced the same
                // rotation (harmless: recipients only rotate on an actual
                // value change).
                if !report.rotated.is_empty() {
                    if let Some(client) = &publish_client {
                        if let Ok(mut conn) = client.get_async_connection().await {
                            let now_ms = chrono::Utc::now().timestamp_millis();
                            let _: std::result::Result<(), _> = redis::cmd("PUBLISH")
                                .arg(ROTATION_CHANNEL)
                                .arg(now_ms.to_string())
                                .query_async(&mut conn)
                                .await;
                        }
                    }
                }
            }
        });
    }

    /// Best-effort: connects and subscribes to `ROTATION_CHANNEL`, returning
    /// `None` (rather than erroring) on any failure so callers can fall back
    /// to poll-only detection.
    async fn connect_rotation_pubsub(redis_url: &str) -> Option<redis::aio::PubSub> {
        let client = match redis::Client::open(redis_url) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("secrets_rotation: failed to build Redis client: {e}");
                return None;
            }
        };
        let conn = match client.get_async_connection().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    "secrets_rotation: pub/sub connection unavailable, falling back \
                     to poll-only detection: {e}"
                );
                return None;
            }
        };
        let mut pubsub = conn.into_pubsub();
        if let Err(e) = pubsub.subscribe(ROTATION_CHANNEL).await {
            tracing::warn!("secrets_rotation: failed to subscribe to {ROTATION_CHANNEL}: {e}");
            return None;
        }
        tracing::info!(
            "secrets_rotation: subscribed to {ROTATION_CHANNEL} for fleet-wide \
             rotation notifications"
        );
        Some(pubsub)
    }
}

/// Simple secret retrieval from environment variables with caching
pub mod env_secrets {
    use std::collections::HashMap;
    use std::sync::{Arc, RwLock};

    #[derive(Clone)]
    pub struct EnvSecretsManager {
        cache: Arc<RwLock<HashMap<String, String>>>,
    }

    impl EnvSecretsManager {
        pub fn new() -> Self {
            Self {
                cache: Arc::new(RwLock::new(HashMap::new())),
            }
        }

        pub fn get_secret(&self, key: &str) -> Result<String, String> {
            // Check cache first
            {
                let cache = self.cache.read().unwrap();
                if let Some(value) = cache.get(key) {
                    return Ok(value.clone());
                }
            }

            // Retrieve from environment
            let value = std::env::var(key).map_err(|_| format!("Secret '{key}' not found"))?;

            // Cache the value
            {
                let mut cache = self.cache.write().unwrap();
                cache.insert(key.to_string(), value.clone());
            }

            Ok(value)
        }

        pub fn rotate_secret(&self, key: &str, new_value: String) {
            let mut cache = self.cache.write().unwrap();
            cache.insert(key.to_string(), new_value);
        }

        pub fn clear_cache(&self) {
            let mut cache = self.cache.write().unwrap();
            cache.clear();
        }

        pub fn cache_size(&self) -> usize {
            let cache = self.cache.read().unwrap();
            cache.len()
        }
    }

    impl Default for EnvSecretsManager {
        fn default() -> Self {
            Self::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::env_secrets::EnvSecretsManager;
    use std::env;

    #[test]
    fn test_secret_retrieval_from_env() {
        // Set up test environment variable
        env::set_var("TEST_SECRET_KEY", "test_secret_value");

        let manager = EnvSecretsManager::new();
        let result = manager.get_secret("TEST_SECRET_KEY");

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "test_secret_value");

        // Clean up
        env::remove_var("TEST_SECRET_KEY");
    }

    #[test]
    fn test_secret_caching() {
        // Set up test environment variable
        env::set_var("CACHED_SECRET", "cached_value");

        let manager = EnvSecretsManager::new();

        // First retrieval - should cache
        let result1 = manager.get_secret("CACHED_SECRET");
        assert!(result1.is_ok());
        assert_eq!(manager.cache_size(), 1);

        // Remove from environment
        env::remove_var("CACHED_SECRET");

        // Second retrieval - should use cache
        let result2 = manager.get_secret("CACHED_SECRET");
        assert!(result2.is_ok());
        assert_eq!(result2.unwrap(), "cached_value");
    }

    #[test]
    fn test_secret_missing_error() {
        let manager = EnvSecretsManager::new();

        // Try to get non-existent secret
        let result = manager.get_secret("NON_EXISTENT_SECRET");

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("Secret 'NON_EXISTENT_SECRET' not found"));
    }

    #[test]
    fn test_secret_rotation() {
        // Set up initial secret
        env::set_var("ROTATABLE_SECRET", "old_value");

        let manager = EnvSecretsManager::new();

        // Get initial value
        let result1 = manager.get_secret("ROTATABLE_SECRET");
        assert_eq!(result1.unwrap(), "old_value");

        // Rotate secret
        manager.rotate_secret("ROTATABLE_SECRET", "new_value".to_string());

        // Get rotated value
        let result2 = manager.get_secret("ROTATABLE_SECRET");
        assert_eq!(result2.unwrap(), "new_value");

        // Clean up
        env::remove_var("ROTATABLE_SECRET");
    }

    #[test]
    fn test_cache_clear() {
        env::set_var("CLEAR_TEST_1", "value1");
        env::set_var("CLEAR_TEST_2", "value2");

        let manager = EnvSecretsManager::new();

        // Cache multiple secrets
        manager.get_secret("CLEAR_TEST_1").unwrap();
        manager.get_secret("CLEAR_TEST_2").unwrap();
        assert_eq!(manager.cache_size(), 2);

        // Clear cache
        manager.clear_cache();
        assert_eq!(manager.cache_size(), 0);

        // Clean up
        env::remove_var("CLEAR_TEST_1");
        env::remove_var("CLEAR_TEST_2");
    }

    #[test]
    fn test_multiple_secret_retrievals() {
        env::set_var("SECRET_1", "value1");
        env::set_var("SECRET_2", "value2");
        env::set_var("SECRET_3", "value3");

        let manager = EnvSecretsManager::new();

        let result1 = manager.get_secret("SECRET_1");
        let result2 = manager.get_secret("SECRET_2");
        let result3 = manager.get_secret("SECRET_3");

        assert_eq!(result1.unwrap(), "value1");
        assert_eq!(result2.unwrap(), "value2");
        assert_eq!(result3.unwrap(), "value3");
        assert_eq!(manager.cache_size(), 3);

        // Clean up
        env::remove_var("SECRET_1");
        env::remove_var("SECRET_2");
        env::remove_var("SECRET_3");
    }

    #[test]
    fn test_concurrent_access() {
        use std::sync::Arc;
        use std::thread;

        env::set_var("CONCURRENT_SECRET", "concurrent_value");

        let manager = Arc::new(EnvSecretsManager::new());
        let mut handles = vec![];

        // Spawn multiple threads accessing the same secret
        for _ in 0..10 {
            let manager_clone = Arc::clone(&manager);
            let handle = thread::spawn(move || {
                let result = manager_clone.get_secret("CONCURRENT_SECRET");
                assert!(result.is_ok());
                assert_eq!(result.unwrap(), "concurrent_value");
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Clean up
        env::remove_var("CONCURRENT_SECRET");
    }
}

/// Part D regression tests: fleet-wide rotation coordination.
#[cfg(test)]
mod rotation_tests {
    use super::*;

    /// A caller presenting the old secret must be accepted during the grace
    /// period and rejected once it elapses — the bound the coordination fix
    /// exists to make actually hold fleet-wide (see the module-level doc on
    /// `REFRESH_INTERVAL` for why two independently-clocked instances could
    /// previously double this window).
    #[test]
    fn verify_rejects_previous_value_once_grace_period_elapses() {
        let mut secret = RotatingSecret::new("v1".to_string());
        assert_eq!(secret.verify("v1"), Some(true));
        assert_eq!(secret.verify("v0"), None);

        secret.rotate("v2".to_string());
        assert_eq!(secret.verify("v2"), Some(true));
        assert_eq!(
            secret.verify("v1"),
            Some(false),
            "previous value should still verify within the grace period"
        );

        // Back-date the rotation timestamp instead of sleeping for real —
        // simulates the grace period having elapsed.
        secret.previous = secret.previous.take().map(|(v, _)| {
            (
                v,
                Instant::now() - ROTATION_GRACE_PERIOD - Duration::from_secs(1),
            )
        });

        assert_eq!(
            secret.verify("v1"),
            None,
            "old secret must stop being accepted once the grace period has elapsed"
        );
        assert_eq!(secret.verify("v2"), Some(true));
    }

    /// Simulates two fleet instances whose `RotatingSecret` state is
    /// coordinated (both rotate to the same new value within milliseconds of
    /// each other via pub/sub, not up to `REFRESH_INTERVAL` apart) and
    /// confirms the old secret is rejected on *both* once the shared grace
    /// period elapses — the previously-unbounded fleet-wide window this
    /// fixes is exactly the gap between independently-clocked instances.
    #[test]
    fn staggered_instances_both_reject_stale_secret_after_shared_grace_period() {
        let mut instance_a = RotatingSecret::new("old-secret".to_string());
        let mut instance_b = RotatingSecret::new("old-secret".to_string());

        // Instance A detects the rotation (e.g. via its own poll)...
        instance_a.rotate("new-secret".to_string());
        // ...and instance B applies the same rotation shortly after, via the
        // pub/sub notification A's poll loop announces — not on B's own
        // independent REFRESH_INTERVAL clock.
        instance_b.rotate("new-secret".to_string());

        for instance in [&instance_a, &instance_b] {
            assert_eq!(instance.verify("new-secret"), Some(true));
            assert_eq!(
                instance.verify("old-secret"),
                Some(false),
                "old secret should still verify during the shared grace period"
            );
        }

        for instance in [&mut instance_a, &mut instance_b] {
            instance.previous = instance.previous.take().map(|(v, _)| {
                (
                    v,
                    Instant::now() - ROTATION_GRACE_PERIOD - Duration::from_secs(1),
                )
            });
        }

        for instance in [&instance_a, &instance_b] {
            assert_eq!(
                instance.verify("old-secret"),
                None,
                "old secret must be rejected on every instance once the shared \
                 grace period elapses, not just the one that detected it first"
            );
        }
    }

    /// Task item 3: the pub/sub connect helper must degrade to `None`
    /// (fallback to poll-only) rather than error/panic when Redis is
    /// unreachable.
    #[tokio::test]
    async fn connect_rotation_pubsub_falls_back_gracefully_when_redis_unreachable() {
        let result =
            SecretsManager::connect_rotation_pubsub("redis://invalid-host-xyz-12345:6379").await;
        assert!(
            result.is_none(),
            "pub/sub connect must return None, not panic or hang, when Redis is unreachable"
        );
    }

    /// Real end-to-end roundtrip through Redis: a message published to
    /// `ROTATION_CHANNEL` from one connection is received by a subscriber
    /// created via `connect_rotation_pubsub`, with the publish timestamp
    /// intact (the same value `start_refresh_task` uses to compute
    /// `secrets_rotation_detection_lag_ms`).
    #[ignore = "Requires Redis"]
    #[tokio::test]
    async fn rotation_pubsub_roundtrip() {
        let redis_url =
            std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());

        let mut pubsub = match SecretsManager::connect_rotation_pubsub(&redis_url).await {
            Some(p) => p,
            None => {
                println!("Skipping: Redis not available");
                return;
            }
        };

        let publisher = redis::Client::open(redis_url.as_str()).unwrap();
        let mut publish_conn = publisher.get_async_connection().await.unwrap();

        // Give the SUBSCRIBE a moment to be registered server-side before publishing.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let published_at_ms = chrono::Utc::now().timestamp_millis();
        let _: i64 = redis::cmd("PUBLISH")
            .arg(ROTATION_CHANNEL)
            .arg(published_at_ms.to_string())
            .query_async(&mut publish_conn)
            .await
            .unwrap();

        let mut stream = pubsub.on_message();
        let msg = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("should receive the published rotation notification within 5s")
            .expect("stream should yield a message, not end");
        let payload: String = msg.get_payload().unwrap();
        assert_eq!(
            payload.parse::<i64>().unwrap(),
            published_at_ms,
            "received payload should be the exact publish timestamp used for lag calculation"
        );
    }

    #[tokio::test]
    async fn webhook_endpoint_secret_rotation_workflow() {
        let store = SecretsStore::new(
            "initial-webhook-secret".to_string(),
            "initial-admin-key".to_string(),
        );

        let initial_secrets = store.valid_webhook_secrets().await;
        assert_eq!(initial_secrets.len(), 1);
        assert_eq!(initial_secrets[0], "initial-webhook-secret");
    }

    #[tokio::test]
    async fn webhook_secret_rotation_and_grace_period() {
        let store = SecretsStore::new("webhook-v1".to_string(), "admin-v1".to_string());

        assert!(store.verify_webhook_secret("webhook-v1").await);
        assert!(!store.verify_webhook_secret("webhook-v2").await);

        let mut secret = store.anchor_webhook_secret.write().await;
        secret.rotate("webhook-v2".to_string());
        drop(secret);

        let valid_secrets = store.valid_webhook_secrets().await;
        assert_eq!(valid_secrets.len(), 2);
        assert!(valid_secrets.contains(&"webhook-v2".to_string()));
        assert!(valid_secrets.contains(&"webhook-v1".to_string()));

        assert!(store.verify_webhook_secret("webhook-v1").await);
        assert!(store.verify_webhook_secret("webhook-v2").await);
    }

    #[tokio::test]
    async fn webhook_secret_expires_after_grace_period() {
        let store = SecretsStore::new("webhook-old".to_string(), "admin-key".to_string());

        let mut secret = store.anchor_webhook_secret.write().await;
        secret.rotate("webhook-new".to_string());

        secret.previous = secret.previous.take().map(|(v, _)| {
            (
                v,
                Instant::now() - ROTATION_GRACE_PERIOD - Duration::from_secs(1),
            )
        });
        drop(secret);

        assert!(!store.verify_webhook_secret("webhook-old").await);
        assert!(store.verify_webhook_secret("webhook-new").await);
    }

    #[tokio::test]
    async fn multiple_webhook_secret_rotations() {
        let store = SecretsStore::new("webhook-v1".to_string(), "admin-v1".to_string());

        let mut secret = store.anchor_webhook_secret.write().await;
        secret.rotate("webhook-v2".to_string());
        secret.rotate("webhook-v3".to_string());
        drop(secret);

        assert!(store.verify_webhook_secret("webhook-v3").await);
        assert!(!store.verify_webhook_secret("webhook-v1").await);
    }

    #[tokio::test]
    async fn secrets_store_thread_safe_webhook_rotation() {
        let store = SecretsStore::new("webhook-initial".to_string(), "admin-initial".to_string());

        let store_clone = store.clone();
        let handle = tokio::spawn(async move {
            let mut secret = store_clone.anchor_webhook_secret.write().await;
            secret.rotate("webhook-updated".to_string());
        });

        handle.await.unwrap();

        let updated_secrets = store.valid_webhook_secrets().await;
        assert!(updated_secrets.contains(&"webhook-updated".to_string()));
    }
}

/// #1336: bounded, telemetered fallback when Vault becomes unreachable at
/// runtime. Time is simulated by evaluating at `start + outage` rather than
/// sleeping.
#[cfg(test)]
mod vault_outage_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    const MAX_AGE: Duration = Duration::from_secs(900);

    fn store() -> SecretsStore {
        SecretsStore::with_fallback_config(
            "webhook-v1".into(),
            "admin-v1".into(),
            VaultFallbackConfig::clamped(Some(MAX_AGE), None),
        )
    }

    /// Fake Vault: returns fixed values, or fails while `down` is set.
    struct FakeVault {
        down: AtomicBool,
        webhook: String,
        admin: String,
    }

    #[async_trait::async_trait]
    impl SecretSource for FakeVault {
        async fn fetch(&self, kind: SecretKind) -> Result<String> {
            if self.down.load(Ordering::SeqCst) {
                anyhow::bail!("connection refused (simulated Vault outage)");
            }
            Ok(match kind {
                SecretKind::AnchorWebhookSecret => self.webhook.clone(),
                SecretKind::AdminApiKey => self.admin.clone(),
            })
        }
    }

    #[test]
    fn config_defaults_are_conservative_and_clamped() {
        let d = VaultFallbackConfig::clamped(None, None);
        assert_eq!(d.max_age, DEFAULT_FALLBACK_MAX_AGE);
        assert_eq!(d.outage_retry_interval, DEFAULT_OUTAGE_RETRY_INTERVAL);
        // Too short would expire secrets between healthy polls.
        let short = VaultFallbackConfig::clamped(
            Some(Duration::from_secs(10)),
            Some(Duration::from_secs(1)),
        );
        assert_eq!(short.max_age, MIN_FALLBACK_MAX_AGE);
        assert_eq!(short.outage_retry_interval, Duration::from_secs(5));
        // Configuration can never make the window unbounded.
        let long = VaultFallbackConfig::clamped(
            Some(Duration::from_secs(86_400)),
            Some(Duration::from_secs(9_999)),
        );
        assert_eq!(long.max_age, MAX_FALLBACK_MAX_AGE);
        assert_eq!(long.outage_retry_interval, REFRESH_INTERVAL);
        assert!(MIN_FALLBACK_MAX_AGE > REFRESH_INTERVAL);
    }

    #[test]
    fn only_long_lived_approved_secrets_may_fall_back() {
        assert!(SecretClass::LongLivedApproved.fallback_allowed());
        assert!(!SecretClass::ShortLived.fallback_allowed());
        assert!(!SecretClass::SingleUse.fallback_allowed());
        for kind in SecretKind::ALL {
            assert_eq!(
                kind.class(),
                SecretClass::LongLivedApproved,
                "{kind:?} must be reviewed"
            );
        }
    }

    #[test]
    fn short_lived_and_single_use_secrets_are_never_served_stale() {
        let now = Instant::now();
        let mut f = Freshness::new(now);
        f.failing_since = Some(now);
        for class in [SecretClass::ShortLived, SecretClass::SingleUse] {
            assert!(matches!(
                usability(class, &f, MAX_AGE, now + Duration::from_secs(1)),
                SecretUsability::Expired { .. }
            ));
        }
        // While refreshes succeed they are fine.
        let healthy = Freshness::new(now);
        assert_eq!(
            usability(SecretClass::SingleUse, &healthy, MAX_AGE, now),
            SecretUsability::Fresh
        );
    }

    /// Outage at various durations: fresh at the start, cached fallback
    /// within the window, hard failure past the max age.
    #[test]
    fn outage_duration_table() {
        let s = store();
        let t0 = Instant::now();
        s.record_refresh_success(SecretKind::AdminApiKey, t0);
        s.record_refresh_failure(SecretKind::AdminApiKey, t0, &"down");
        let cases = [
            (0u64, "fallback"),
            (60, "fallback"),
            (299, "fallback"),
            (600, "fallback"),
            (899, "fallback"),
            (900, "fallback"),
            (901, "expired"),
            (3_600, "expired"),
        ];
        for (secs, expected) in cases {
            let u = s.usability_at(SecretKind::AdminApiKey, t0 + Duration::from_secs(secs));
            let got = match u {
                SecretUsability::Fresh => "fresh",
                SecretUsability::CachedFallback { remaining, .. } => {
                    assert_eq!(remaining, MAX_AGE - Duration::from_secs(secs));
                    "fallback"
                }
                SecretUsability::Expired { .. } => "expired",
            };
            assert_eq!(got, expected, "outage of {secs}s");
        }
    }

    #[tokio::test]
    async fn within_window_verification_keeps_working() {
        let s = store();
        let t0 = Instant::now();
        for kind in SecretKind::ALL {
            s.record_refresh_success(kind, t0);
            s.record_refresh_failure(kind, t0, &"down");
        }
        let during = t0 + Duration::from_secs(300);
        assert!(
            s.verify_and_record(SecretKind::AdminApiKey, "admin-v1", during)
                .await
        );
        assert!(
            !s.verify_and_record(SecretKind::AdminApiKey, "wrong", during)
                .await
        );
        assert_eq!(
            s.valid_values_at(SecretKind::AnchorWebhookSecret, during)
                .await,
            vec!["webhook-v1".to_string()]
        );
        let status = s.vault_status_at(during);
        assert_eq!(status.status, "degraded_cached_fallback");
        assert_eq!(status.unreachable_for_secs, Some(300));
        assert_eq!(status.fallback_remaining_secs, Some(600));
        assert_eq!(status.max_fallback_age_secs, 900);
    }

    #[tokio::test]
    async fn beyond_window_fails_closed_even_with_the_right_secret() {
        let s = store();
        let t0 = Instant::now();
        s.record_refresh_failure(SecretKind::AdminApiKey, t0, &"down");
        s.record_refresh_failure(SecretKind::AnchorWebhookSecret, t0, &"down");
        let after = t0 + MAX_AGE + Duration::from_secs(1);
        // A second failure after expiry exercises the one-shot expiry log.
        s.record_refresh_failure(SecretKind::AdminApiKey, after, &"down");
        s.record_refresh_failure(SecretKind::AdminApiKey, after, &"down");
        assert!(
            !s.verify_and_record(SecretKind::AdminApiKey, "admin-v1", after)
                .await
        );
        assert!(
            !s.verify_and_record(SecretKind::AnchorWebhookSecret, "webhook-v1", after)
                .await
        );
        assert!(s
            .valid_values_at(SecretKind::AnchorWebhookSecret, after)
            .await
            .is_empty());
        let status = s.vault_status_at(after);
        assert_eq!(status.status, "expired");
        assert!(status.secrets.iter().all(|r| r.status == "expired"));
    }

    #[tokio::test]
    async fn recovery_restores_fresh_state() {
        let s = store();
        let t0 = Instant::now();
        s.record_refresh_failure(SecretKind::AdminApiKey, t0, &"down");
        let later = t0 + MAX_AGE * 2;
        assert!(matches!(
            s.usability_at(SecretKind::AdminApiKey, later),
            SecretUsability::Expired { .. }
        ));
        for kind in SecretKind::ALL {
            s.record_refresh_success(kind, later);
        }
        assert_eq!(
            s.usability_at(SecretKind::AdminApiKey, later),
            SecretUsability::Fresh
        );
        assert!(
            s.verify_and_record(SecretKind::AdminApiKey, "admin-v1", later)
                .await
        );
        assert_eq!(s.vault_status_at(later).status, "ok");
    }

    /// A refresh loop that silently stopped must not keep secrets valid
    /// forever either.
    #[test]
    fn stalled_refresh_loop_is_bounded_too() {
        let s = store();
        let t0 = Instant::now();
        s.record_refresh_success(SecretKind::AnchorWebhookSecret, t0);
        assert_eq!(
            s.usability_at(SecretKind::AnchorWebhookSecret, t0 + REFRESH_INTERVAL * 2),
            SecretUsability::Fresh
        );
        assert!(matches!(
            s.usability_at(
                SecretKind::AnchorWebhookSecret,
                t0 + REFRESH_INTERVAL * 2 + Duration::from_secs(1)
            ),
            SecretUsability::CachedFallback { .. }
        ));
        assert!(matches!(
            s.usability_at(
                SecretKind::AnchorWebhookSecret,
                t0 + MAX_AGE + Duration::from_secs(1)
            ),
            SecretUsability::Expired { .. }
        ));
    }

    #[tokio::test]
    async fn refresh_once_rotates_records_failures_and_recovers() {
        let s = store();
        let vault = FakeVault {
            down: AtomicBool::new(false),
            webhook: "webhook-v2".into(),
            admin: "admin-v1".into(),
        };

        let report = refresh_once(&vault, &s, 0.0, false).await;
        assert_eq!(report.rotated, vec![SecretKind::AnchorWebhookSecret]);
        assert!(report.failed.is_empty());
        assert!(s.verify_webhook_secret("webhook-v2").await);

        vault.down.store(true, Ordering::SeqCst);
        let report = refresh_once(&vault, &s, 0.0, false).await;
        assert_eq!(report.failed, SecretKind::ALL.to_vec());
        assert_eq!(s.vault_status().status, "degraded_cached_fallback");
        // Still within the window right now: verification keeps working.
        assert!(s.verify_admin_key("admin-v1").await);
        assert_eq!(s.valid_admin_keys().await, vec!["admin-v1".to_string()]);

        vault.down.store(false, Ordering::SeqCst);
        let report = refresh_once(&vault, &s, 0.0, true).await;
        assert!(report.failed.is_empty());
        assert_eq!(s.vault_status().status, "ok");
    }

    #[test]
    fn vault_errors_classify_for_the_scorecard() {
        use crate::services::dependency_scorecard::CallOutcome;
        let api = anyhow::Error::new(vaultrs::error::ClientError::APIError {
            code: 503,
            errors: vec![],
        });
        assert_eq!(classify_vault_error(&api), CallOutcome::DependencyFault);
        let denied = anyhow::Error::new(vaultrs::error::ClientError::APIError {
            code: 403,
            errors: vec![],
        });
        assert_eq!(classify_vault_error(&denied), CallOutcome::Success);
        let wrapped = anyhow::Error::new(vaultrs::error::ClientError::APIError {
            code: 500,
            errors: vec![],
        })
        .context("failed to read secret/admin from Vault");
        assert_eq!(classify_vault_error(&wrapped), CallOutcome::DependencyFault);
        assert_eq!(
            classify_vault_error(&anyhow::anyhow!("opaque")),
            CallOutcome::TransportFailure
        );
    }

    #[test]
    fn gauges_register_without_panicking() {
        let _g = store().register_vault_gauges();
    }
}
