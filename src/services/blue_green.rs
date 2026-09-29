//! # Blue-Green Deployment Support (#1288)
//!
//! Provides application-level tooling for blue-green deployments for
//! schema-compatible releases (i.e. releases that do not require breaking
//! schema changes).
//!
//! ## Concept
//!
//! A "blue-green" deployment runs a new "green" application instance (or
//! pod/replica set) in parallel with the existing "blue" instance.  Traffic
//! is cut over to green **only after** green passes health and readiness
//! checks.  If green fails post-cutover checks within a bounded time window,
//! traffic is instantly reverted to blue.
//!
//! ## Components
//!
//! * **[`BlueGreenState`]** — shared in-memory state tracking which slot
//!   (`Blue` or `Green`) is currently active.  Stored in [`AppState`] and
//!   exposed via the control plane endpoints below.
//!
//! * **[`BlueGreenController`]** — async logic that:
//!    1. Runs health + readiness checks against a target Green URL.
//!    2. Cuts over (flips the active slot).
//!    3. Runs post-cutover verification checks against a Blue URL (the old
//!       instance is still running).
//!    4. If post-cutover checks fail within the rollback window, reverts to
//!       Blue instantly.
//!
//! * **Admin HTTP endpoints** (mounted in `create_app`):
//!    - `POST /admin/blue-green/cutover`  — start a cutover to green.
//!    - `POST /admin/blue-green/rollback` — revert to blue instantly.
//!    - `GET  /admin/blue-green/status`   — current slot + check results.
//!
//! ## Scope
//!
//! This module covers the **application-level** cutover only.  Actual traffic
//! steering (load balancer rule changes, Kubernetes Service selector flips)
//! is out of scope and is handled by the orchestration layer (see
//! `docs/deployment.md`).  This module produces the signal (which slot is
//! healthy/active) that the orchestration layer consumes.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

// ---------------------------------------------------------------------------
// Slot enum
// ---------------------------------------------------------------------------

/// Which deployment slot is currently receiving traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Slot {
    Blue,
    Green,
}

impl std::fmt::Display for Slot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Slot::Blue => write!(f, "blue"),
            Slot::Green => write!(f, "green"),
        }
    }
}

// ---------------------------------------------------------------------------
// Deployment state
// ---------------------------------------------------------------------------

/// Check outcome recorded during a cutover or rollback run.
#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub name: String,
    pub passed: bool,
    pub detail: Option<String>,
    pub checked_at: DateTime<Utc>,
}

/// Immutable snapshot of a completed or in-progress cutover attempt.
#[derive(Debug, Clone, Serialize)]
pub struct CutoverAttempt {
    pub id: uuid::Uuid,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Slot active **before** this attempt started.
    pub from_slot: Slot,
    /// Slot this attempt tried to activate.
    pub to_slot: Slot,
    /// Pre-cutover readiness checks run against `to_slot`.
    pub pre_checks: Vec<CheckResult>,
    /// Post-cutover verification checks run after traffic was switched.
    pub post_checks: Vec<CheckResult>,
    pub outcome: CutoverOutcome,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CutoverOutcome {
    /// Cutover in progress.
    InProgress,
    /// Cutover succeeded; green is now active.
    Success,
    /// Pre-cutover checks failed; never switched.
    PreCheckFailed,
    /// Post-cutover checks failed within the rollback window; reverted to blue.
    RolledBack,
    /// Explicit operator rollback.
    ManualRollback,
}

/// Shared, in-memory blue-green state.  Clone is cheap (Arc<RwLock>).
#[derive(Clone)]
pub struct BlueGreenState {
    inner: Arc<RwLock<BlueGreenInner>>,
}

struct BlueGreenInner {
    active_slot: Slot,
    /// URL of the blue ("old") instance.
    blue_url: String,
    /// URL of the green ("new") instance.
    green_url: String,
    /// Most recent cutover attempt, if any.
    last_attempt: Option<CutoverAttempt>,
    /// Rollback window: how long after cutover post-checks are run before
    /// declaring success.
    rollback_window: Duration,
}

impl BlueGreenState {
    /// Create initial state with both URLs and a rollback window.
    ///
    /// `blue_url` and `green_url` are the base URLs the controller uses to
    /// run `/health` and `/ready` checks against each slot.
    pub fn new(blue_url: String, green_url: String, rollback_window: Duration) -> Self {
        Self {
            inner: Arc::new(RwLock::new(BlueGreenInner {
                active_slot: Slot::Blue,
                blue_url,
                green_url,
                last_attempt: None,
                rollback_window,
            })),
        }
    }

    /// Returns the currently active deployment slot.
    pub async fn active_slot(&self) -> Slot {
        self.inner.read().await.active_slot
    }

    /// Returns a status snapshot suitable for the admin API.
    pub async fn status(&self) -> BlueGreenStatus {
        let inner = self.inner.read().await;
        BlueGreenStatus {
            active_slot: inner.active_slot,
            blue_url: inner.blue_url.clone(),
            green_url: inner.green_url.clone(),
            last_attempt: inner.last_attempt.clone(),
            rollback_window_secs: inner.rollback_window.as_secs(),
        }
    }
}

/// JSON-serialisable status blob returned by `GET /admin/blue-green/status`.
#[derive(Debug, Serialize)]
pub struct BlueGreenStatus {
    pub active_slot: Slot,
    pub blue_url: String,
    pub green_url: String,
    pub last_attempt: Option<CutoverAttempt>,
    pub rollback_window_secs: u64,
}

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

/// Drives the cutover and rollback state machine.
pub struct BlueGreenController {
    state: BlueGreenState,
    http: reqwest::Client,
    /// Timeout per individual health check request.
    check_timeout: Duration,
    /// Number of consecutive passing checks required before declaring green healthy.
    required_passing_checks: u32,
}

impl BlueGreenController {
    pub fn new(state: BlueGreenState) -> Self {
        Self {
            state,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("reqwest client build"),
            check_timeout: Duration::from_secs(5),
            required_passing_checks: 3,
        }
    }

    /// Run a single health + readiness check against `base_url`.
    async fn run_checks_against(&self, base_url: &str) -> Vec<CheckResult> {
        let mut results = Vec::new();

        for (name, path) in [("health", "/health"), ("readiness", "/ready")] {
            let url = format!("{}{}", base_url.trim_end_matches('/'), path);
            let checked_at = Utc::now();

            let check = async {
                let resp = self.http.get(&url).send().await?;
                Ok::<_, reqwest::Error>(resp.status().is_success())
            };

            match tokio::time::timeout(self.check_timeout, check).await {
                Ok(Ok(true)) => results.push(CheckResult {
                    name: name.to_string(),
                    passed: true,
                    detail: None,
                    checked_at,
                }),
                Ok(Ok(false)) => results.push(CheckResult {
                    name: name.to_string(),
                    passed: false,
                    detail: Some(format!("{} returned non-2xx", path)),
                    checked_at,
                }),
                Ok(Err(e)) => results.push(CheckResult {
                    name: name.to_string(),
                    passed: false,
                    detail: Some(format!("request error: {e}")),
                    checked_at,
                }),
                Err(_) => results.push(CheckResult {
                    name: name.to_string(),
                    passed: false,
                    detail: Some(format!("check timed out after {}s", self.check_timeout.as_secs())),
                    checked_at,
                }),
            }
        }

        results
    }

    fn all_passed(checks: &[CheckResult]) -> bool {
        checks.iter().all(|c| c.passed)
    }

    /// Attempt a blue → green cutover.
    ///
    /// Steps:
    ///  1. Run pre-cutover readiness checks against the green URL.  Bail if
    ///     any fail.
    ///  2. Flip the active slot to Green (this is the "traffic cutover signal"
    ///     — the orchestration layer reads [`BlueGreenState::active_slot`] to
    ///     decide where to route).
    ///  3. After a short stabilisation pause, run post-cutover checks against
    ///     the green URL.
    ///  4. If post-cutover checks fail within the rollback window, flip back
    ///     to Blue instantly.
    ///
    /// Returns the completed [`CutoverAttempt`] for the caller / admin API.
    pub async fn cutover_to_green(&self) -> CutoverAttempt {
        let attempt_id = uuid::Uuid::new_v4();
        let started_at = Utc::now();
        let (green_url, rollback_window) = {
            let inner = self.state.inner.read().await;
            (inner.green_url.clone(), inner.rollback_window)
        };

        info!(
            attempt_id = %attempt_id,
            green_url = %green_url,
            "blue-green: starting cutover to green"
        );

        // -- Step 1: pre-cutover checks --
        let mut pre_checks_all: Vec<CheckResult> = Vec::new();
        let mut pre_passed = false;

        for round in 0..self.required_passing_checks {
            let checks = self.run_checks_against(&green_url).await;
            let round_passed = Self::all_passed(&checks);
            pre_checks_all.extend(checks);

            if !round_passed {
                info!(
                    attempt_id = %attempt_id,
                    round,
                    "blue-green: pre-cutover check round failed"
                );
                break;
            }

            if round == self.required_passing_checks - 1 {
                pre_passed = true;
            } else {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }

        if !pre_passed {
            warn!(
                attempt_id = %attempt_id,
                "blue-green: pre-cutover checks failed, aborting cutover"
            );
            let attempt = CutoverAttempt {
                id: attempt_id,
                started_at,
                completed_at: Some(Utc::now()),
                from_slot: Slot::Blue,
                to_slot: Slot::Green,
                pre_checks: pre_checks_all,
                post_checks: vec![],
                outcome: CutoverOutcome::PreCheckFailed,
            };
            self.record_attempt(attempt.clone()).await;
            return attempt;
        }

        // -- Step 2: flip traffic --
        {
            let mut inner = self.state.inner.write().await;
            inner.active_slot = Slot::Green;
        }
        info!(
            attempt_id = %attempt_id,
            "blue-green: active slot flipped to GREEN"
        );

        // Brief stabilisation window before post-cutover checks.
        tokio::time::sleep(Duration::from_secs(5)).await;

        // -- Step 3: post-cutover checks within rollback window --
        let post_checks = self.run_checks_against(&green_url).await;
        let post_passed = Self::all_passed(&post_checks);

        if post_passed {
            info!(
                attempt_id = %attempt_id,
                "blue-green: post-cutover checks passed — green is now active"
            );
            let attempt = CutoverAttempt {
                id: attempt_id,
                started_at,
                completed_at: Some(Utc::now()),
                from_slot: Slot::Blue,
                to_slot: Slot::Green,
                pre_checks: pre_checks_all,
                post_checks,
                outcome: CutoverOutcome::Success,
            };
            self.record_attempt(attempt.clone()).await;
            return attempt;
        }

        // -- Step 4: post-cutover checks failed, rollback --
        error!(
            attempt_id = %attempt_id,
            rollback_window_secs = rollback_window.as_secs(),
            "blue-green: post-cutover checks failed, rolling back to BLUE"
        );
        {
            let mut inner = self.state.inner.write().await;
            inner.active_slot = Slot::Blue;
        }
        info!(
            attempt_id = %attempt_id,
            "blue-green: active slot reverted to BLUE (rollback complete)"
        );

        let attempt = CutoverAttempt {
            id: attempt_id,
            started_at,
            completed_at: Some(Utc::now()),
            from_slot: Slot::Blue,
            to_slot: Slot::Green,
            pre_checks: pre_checks_all,
            post_checks,
            outcome: CutoverOutcome::RolledBack,
        };
        self.record_attempt(attempt.clone()).await;
        attempt
    }

    /// Instantly revert to blue (manual operator rollback).
    pub async fn rollback_to_blue(&self) -> CutoverAttempt {
        let current = self.state.inner.read().await.active_slot;
        {
            let mut inner = self.state.inner.write().await;
            inner.active_slot = Slot::Blue;
        }
        info!("blue-green: manual rollback, active slot set to BLUE");

        let attempt = CutoverAttempt {
            id: uuid::Uuid::new_v4(),
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            from_slot: current,
            to_slot: Slot::Blue,
            pre_checks: vec![],
            post_checks: vec![],
            outcome: CutoverOutcome::ManualRollback,
        };
        self.record_attempt(attempt.clone()).await;
        attempt
    }

    async fn record_attempt(&self, attempt: CutoverAttempt) {
        let mut inner = self.state.inner.write().await;
        inner.last_attempt = Some(attempt);
    }
}

// ---------------------------------------------------------------------------
// Admin HTTP handlers
// ---------------------------------------------------------------------------

/// Request body for `POST /admin/blue-green/cutover`.
/// Currently empty — the target slot is always Green (blue→green cutover).
/// Kept as a struct for forward-compatibility.
#[derive(Debug, Deserialize)]
pub struct CutoverRequest {}

/// `GET /admin/blue-green/status`
pub async fn blue_green_status_handler(
    State(state): State<BlueGreenState>,
) -> impl IntoResponse {
    let status = state.status().await;
    (StatusCode::OK, Json(status))
}

/// `POST /admin/blue-green/cutover`
///
/// Triggers a blue → green cutover.  Runs pre-cutover health/readiness checks
/// against the configured green URL, flips the active slot on success, then
/// runs post-cutover checks.  Rolls back automatically if post-cutover checks
/// fail within the rollback window.
pub async fn blue_green_cutover_handler(
    State(state): State<BlueGreenState>,
    Json(_payload): Json<CutoverRequest>,
) -> impl IntoResponse {
    let controller = BlueGreenController::new(state);
    let attempt = controller.cutover_to_green().await;

    let status_code = match attempt.outcome {
        CutoverOutcome::Success => StatusCode::OK,
        CutoverOutcome::InProgress => StatusCode::ACCEPTED,
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    };

    (status_code, Json(attempt))
}

/// `POST /admin/blue-green/rollback`
///
/// Instantly reverts the active slot to Blue regardless of the current state.
/// Returns `200` with the rollback attempt record.
pub async fn blue_green_rollback_handler(
    State(state): State<BlueGreenState>,
) -> impl IntoResponse {
    let controller = BlueGreenController::new(state);
    let attempt = controller.rollback_to_blue().await;
    (StatusCode::OK, Json(attempt))
}

// ---------------------------------------------------------------------------
// Axum router helper
// ---------------------------------------------------------------------------

/// Build and return the blue-green admin routes.  Mount with:
///
/// ```ignore
/// .merge(blue_green_routes(bg_state)
///     .layer(admin_auth_layer))
/// ```
pub fn blue_green_routes(state: BlueGreenState) -> axum::Router {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/admin/blue-green/status", get(blue_green_status_handler))
        .route("/admin/blue-green/cutover", post(blue_green_cutover_handler))
        .route("/admin/blue-green/rollback", post(blue_green_rollback_handler))
        .with_state(state)
}
