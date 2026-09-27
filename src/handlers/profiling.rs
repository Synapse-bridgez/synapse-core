use crate::error::AppError;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::AppState;

/// Configuration for profiling sessions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfilingConfig {
    /// Duration of profiling in seconds
    pub duration_secs: u64,
    /// Profile type: "cpu" or "memory"
    pub profile_type: String,
    /// Whether to generate flame graph immediately
    pub generate_flamegraph: bool,
    /// Sample rate (Hz) for CPU profiling
    pub sample_rate: Option<u32>,
}

/// A profiling session result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfilingSession {
    pub session_id: String,
    pub start_time: u64,
    pub end_time: Option<u64>,
    pub duration_secs: u64,
    pub profile_type: String,
    pub status: String, // "running", "completed", "failed"
    pub flamegraph_path: Option<String>,
    pub data_size_bytes: Option<u64>,
}

/// Request to start a profiling session
#[derive(Debug, Deserialize)]
pub struct StartProfilingRequest {
    #[serde(default = "default_duration")]
    pub duration_secs: u64,
    #[serde(default = "default_profile_type")]
    pub profile_type: String,
    #[serde(default = "default_generate_flamegraph")]
    pub generate_flamegraph: bool,
    pub sample_rate: Option<u32>,
}

fn default_duration() -> u64 {
    30
}

fn default_profile_type() -> String {
    "cpu".to_string()
}

fn default_generate_flamegraph() -> bool {
    true
}

/// Configuration for continuous (always-on) sampling profiling.
///
/// Unlike an on-demand session, continuous profiling runs indefinitely as a
/// sequence of short rotations, so a low `sample_rate_hz` matters for
/// overhead: the on-demand default is 100 Hz for a single bounded session,
/// but that rate held indefinitely would add meaningfully more sampling
/// interrupt overhead than a rate meant to run 24/7.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ContinuousProfilingConfig {
    /// Sampling rate in Hz. Deliberately low relative to on-demand
    /// profiling's default (100 Hz) to keep steady-state overhead small.
    pub sample_rate_hz: u32,
    /// How often to rotate to a new flamegraph, in seconds.
    pub rotation_secs: u64,
    /// How many completed rotations to retain (rolling window). Older
    /// rotations, and their flamegraph files, are evicted on each rotation.
    pub retention: usize,
}

impl Default for ContinuousProfilingConfig {
    fn default() -> Self {
        Self {
            sample_rate_hz: 19,
            rotation_secs: 300,
            retention: 6, // ~30 minutes of rolling history at the default rotation
        }
    }
}

/// Global profiling state
#[derive(Clone)]
pub struct ProfilingManager {
    is_profiling: Arc<AtomicBool>,
    current_session: Arc<tokio::sync::Mutex<Option<ProfilingSession>>>,
    /// Completed continuous-profiling rotations, oldest first, bounded to
    /// `ContinuousProfilingConfig::retention` entries.
    continuous_sessions: Arc<tokio::sync::Mutex<VecDeque<ProfilingSession>>>,
}

impl ProfilingManager {
    pub fn new() -> Self {
        Self {
            is_profiling: Arc::new(AtomicBool::new(false)),
            current_session: Arc::new(tokio::sync::Mutex::new(None)),
            continuous_sessions: Arc::new(tokio::sync::Mutex::new(VecDeque::new())),
        }
    }

    /// Check if profiling is currently active
    pub fn is_profiling(&self) -> bool {
        self.is_profiling.load(Ordering::Relaxed)
    }

    /// Get the current session if any
    pub async fn get_current_session(&self) -> Option<ProfilingSession> {
        self.current_session.lock().await.clone()
    }

    /// Start a CPU profiling session
    pub async fn start_cpu_profiling(
        &self,
        duration_secs: u64,
        sample_rate: u32,
    ) -> Result<ProfilingSession, String> {
        if self.is_profiling.load(Ordering::Relaxed) {
            return Err("Profiling session already in progress".to_string());
        }

        let session_id = format!(
            "profile-cpu-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );

        let start_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let session = ProfilingSession {
            session_id: session_id.clone(),
            start_time,
            end_time: None,
            duration_secs,
            profile_type: "cpu".to_string(),
            status: "running".to_string(),
            flamegraph_path: None,
            data_size_bytes: None,
        };

        self.is_profiling.store(true, Ordering::Relaxed);
        *self.current_session.lock().await = Some(session.clone());

        // Start the profiler in a background task
        let session_id = session_id.clone();
        let is_profiling = self.is_profiling.clone();
        let current_session = self.current_session.clone();

        tokio::spawn(async move {
            match run_cpu_profiling(&session_id, duration_secs, sample_rate).await {
                Ok(flamegraph_path) => {
                    if let Some(session) = current_session.lock().await.as_mut() {
                        session.status = "completed".to_string();
                        session.end_time = Some(
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_secs(),
                        );
                        session.flamegraph_path = Some(flamegraph_path);

                        if let Ok(metadata) =
                            fs::metadata(session.flamegraph_path.as_ref().unwrap())
                        {
                            session.data_size_bytes = Some(metadata.len());
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("CPU profiling failed: {}", e);
                    if let Some(session) = current_session.lock().await.as_mut() {
                        session.status = format!("failed: {e}");
                        session.end_time = Some(
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_secs(),
                        );
                    }
                }
            }
            is_profiling.store(false, Ordering::Relaxed);
        });

        Ok(session)
    }

    /// Start a memory profiling session
    pub async fn start_memory_profiling(
        &self,
        duration_secs: u64,
    ) -> Result<ProfilingSession, String> {
        if self.is_profiling.load(Ordering::Relaxed) {
            return Err("Profiling session already in progress".to_string());
        }

        let session_id = format!(
            "profile-memory-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );

        let start_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let session = ProfilingSession {
            session_id: session_id.clone(),
            start_time,
            end_time: None,
            duration_secs,
            profile_type: "memory".to_string(),
            status: "running".to_string(),
            flamegraph_path: None,
            data_size_bytes: None,
        };

        self.is_profiling.store(true, Ordering::Relaxed);
        *self.current_session.lock().await = Some(session.clone());

        // Start memory profiling in background
        let session_id = session_id.clone();
        let is_profiling = self.is_profiling.clone();
        let current_session = self.current_session.clone();

        tokio::spawn(async move {
            match run_memory_profiling(&session_id, duration_secs).await {
                Ok(flamegraph_path) => {
                    if let Some(session) = current_session.lock().await.as_mut() {
                        session.status = "completed".to_string();
                        session.end_time = Some(
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_secs(),
                        );
                        session.flamegraph_path = Some(flamegraph_path);

                        if let Ok(metadata) =
                            fs::metadata(session.flamegraph_path.as_ref().unwrap())
                        {
                            session.data_size_bytes = Some(metadata.len());
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Memory profiling failed: {}", e);
                    if let Some(session) = current_session.lock().await.as_mut() {
                        session.status = format!("failed: {e}");
                        session.end_time = Some(
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_secs(),
                        );
                    }
                }
            }
            is_profiling.store(false, Ordering::Relaxed);
        });

        Ok(session)
    }

    /// Stop profiling if any session is in progress (on-demand or continuous).
    pub async fn stop_profiling(&self) -> Result<(), String> {
        if !self.is_profiling.load(Ordering::Relaxed) {
            return Err("No profiling session in progress".to_string());
        }

        self.is_profiling.store(false, Ordering::Relaxed);
        Ok(())
    }

    /// Start continuous, low-overhead sampling profiling.
    ///
    /// Runs indefinitely as a sequence of `rotation_secs`-long CPU sampling
    /// windows at `sample_rate_hz`, retaining the last `retention`
    /// completed rotations (and their flamegraphs) so an operator can
    /// retroactively investigate a performance anomaly after the fact,
    /// rather than needing to already suspect one before it happens.
    ///
    /// Shares the same `is_profiling` exclusivity flag as on-demand
    /// profiling: `pprof::ProfilerGuard` is a single process-wide profiler,
    /// so an on-demand session and continuous profiling can never run
    /// concurrently. Call [`Self::stop_profiling`] to stop it; the running
    /// rotation finishes first (stops within `rotation_secs`).
    pub async fn start_continuous_profiling(
        &self,
        config: ContinuousProfilingConfig,
    ) -> Result<(), String> {
        if self.is_profiling.swap(true, Ordering::Relaxed) {
            return Err("Profiling session already in progress".to_string());
        }

        let is_profiling = self.is_profiling.clone();
        let current_session = self.current_session.clone();
        let continuous_sessions = self.continuous_sessions.clone();

        tokio::spawn(async move {
            tracing::info!(
                sample_rate_hz = config.sample_rate_hz,
                rotation_secs = config.rotation_secs,
                retention = config.retention,
                "Starting continuous CPU profiling"
            );

            while is_profiling.load(Ordering::Relaxed) {
                let session_id = format!(
                    "profile-continuous-{}",
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_millis()
                );
                let start_time = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();

                *current_session.lock().await = Some(ProfilingSession {
                    session_id: session_id.clone(),
                    start_time,
                    end_time: None,
                    duration_secs: config.rotation_secs,
                    profile_type: "continuous".to_string(),
                    status: "running".to_string(),
                    flamegraph_path: None,
                    data_size_bytes: None,
                });

                match run_cpu_profiling(&session_id, config.rotation_secs, config.sample_rate_hz)
                    .await
                {
                    Ok(flamegraph_path) => {
                        let data_size_bytes = fs::metadata(&flamegraph_path).ok().map(|m| m.len());
                        let mut sessions = continuous_sessions.lock().await;
                        sessions.push_back(ProfilingSession {
                            session_id,
                            start_time,
                            end_time: Some(
                                SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .unwrap()
                                    .as_secs(),
                            ),
                            duration_secs: config.rotation_secs,
                            profile_type: "continuous".to_string(),
                            status: "completed".to_string(),
                            flamegraph_path: Some(flamegraph_path),
                            data_size_bytes,
                        });
                        for evicted_path in evict_beyond_retention(&mut sessions, config.retention)
                        {
                            let _ = fs::remove_file(evicted_path);
                        }
                    }
                    Err(e) => {
                        tracing::error!("Continuous profiling rotation failed: {}", e);
                        // Avoid a tight failure loop (e.g. disk full) from
                        // pegging a CPU on its own.
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    }
                }
            }

            *current_session.lock().await = None;
            tracing::info!("Continuous CPU profiling stopped");
        });

        Ok(())
    }

    /// Completed continuous-profiling rotations currently retained, oldest first.
    pub async fn continuous_sessions(&self) -> Vec<ProfilingSession> {
        self.continuous_sessions.lock().await.iter().cloned().collect()
    }
}

/// Evict rotations beyond `retention` from the front (oldest) of `sessions`,
/// returning the flamegraph paths of evicted rotations so the caller can
/// delete the now-unreferenced files and keep on-disk usage bounded to the
/// retention window.
fn evict_beyond_retention(
    sessions: &mut VecDeque<ProfilingSession>,
    retention: usize,
) -> Vec<String> {
    let mut evicted_paths = Vec::new();
    while sessions.len() > retention {
        if let Some(evicted) = sessions.pop_front() {
            if let Some(path) = evicted.flamegraph_path {
                evicted_paths.push(path);
            }
        }
    }
    evicted_paths
}

impl Default for ProfilingManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Run CPU profiling with pprof
async fn run_cpu_profiling(
    session_id: &str,
    duration_secs: u64,
    sample_rate: u32,
) -> Result<String, String> {
    // Ensure profiling output directory exists
    let profile_dir = PathBuf::from("./profiling_data");
    fs::create_dir_all(&profile_dir).map_err(|e| e.to_string())?;

    let guard = pprof::ProfilerGuard::new(sample_rate as i32).map_err(|e| e.to_string())?;

    // Sleep for the specified duration
    tokio::time::sleep(tokio::time::Duration::from_secs(duration_secs)).await;

    // Stop profiling
    match guard.report().build() {
        Ok(report) => {
            let flamegraph_path = profile_dir.join(format!("{session_id}.svg"));
            let flamegraph_file =
                std::fs::File::create(&flamegraph_path).map_err(|e| e.to_string())?;

            report
                .flamegraph(flamegraph_file)
                .map_err(|e| e.to_string())?;

            Ok(flamegraph_path.to_string_lossy().to_string())
        }
        Err(e) => Err(format!("Failed to build profiling report: {e}")),
    }
}

/// Run memory profiling
async fn run_memory_profiling(session_id: &str, duration_secs: u64) -> Result<String, String> {
    // Ensure profiling output directory exists
    let profile_dir = PathBuf::from("./profiling_data");
    fs::create_dir_all(&profile_dir).map_err(|e| e.to_string())?;

    // For memory profiling, we'll collect allocator stats if available
    // This is a placeholder that creates a dummy SVG file
    tokio::time::sleep(tokio::time::Duration::from_secs(duration_secs)).await;

    let flamegraph_path = profile_dir.join(format!("{session_id}.svg"));
    let placeholder_svg = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <svg viewBox=\"0 0 1024 512\" xmlns=\"http://www.w3.org/2000/svg\">\n  \
         <rect width=\"1024\" height=\"512\" fill=\"#f0f0f0\"/>\n  \
         <text x=\"512\" y=\"256\" font-size=\"24\" text-anchor=\"middle\" dominant-baseline=\"middle\">\n    \
         Memory Profiling Session: {session_id}\n  \
         </text>\n  \
         <text x=\"512\" y=\"300\" font-size=\"14\" text-anchor=\"middle\" fill=\"#666\">\n    \
         Memory profiling data would appear here\n  \
         </text>\n\
         </svg>"
    );

    fs::write(&flamegraph_path, placeholder_svg).map_err(|e| e.to_string())?;

    Ok(flamegraph_path.to_string_lossy().to_string())
}

/// HTTP handler to start profiling
pub async fn start_profiling(
    State(state): State<AppState>,
    Json(req): Json<StartProfilingRequest>,
) -> Result<impl IntoResponse, AppError> {
    let profile_type = req.profile_type.to_lowercase();

    if profile_type == "continuous" {
        let mut config = ContinuousProfilingConfig::default();
        if let Some(sample_rate) = req.sample_rate {
            config.sample_rate_hz = sample_rate;
        }
        if req.duration_secs > 0 {
            config.rotation_secs = req.duration_secs;
        }
        return match state
            .profiling_manager
            .start_continuous_profiling(config)
            .await
        {
            Ok(()) => Ok((
                StatusCode::OK,
                Json(json!({ "status": "continuous profiling started", "config": config })),
            )),
            Err(e) => {
                tracing::error!("Failed to start continuous profiling: {}", e);
                Err(AppError::Internal(e))
            }
        };
    }

    let result = match profile_type.as_str() {
        "cpu" => {
            let sample_rate = req.sample_rate.unwrap_or(100);
            state
                .profiling_manager
                .start_cpu_profiling(req.duration_secs, sample_rate)
                .await
        }
        "memory" => {
            state
                .profiling_manager
                .start_memory_profiling(req.duration_secs)
                .await
        }
        _ => Err(format!(
            "Unknown profile type '{profile_type}'. Supported types: cpu, memory, continuous"
        )),
    };

    match result {
        Ok(session) => Ok((StatusCode::OK, Json(json!(session)))),
        Err(e) => {
            tracing::error!("Failed to start profiling: {}", e);
            Err(AppError::Internal(e))
        }
    }
}

/// HTTP handler to get current profiling status
pub async fn get_profiling_status(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let session = state.profiling_manager.get_current_session().await;
    let is_profiling = state.profiling_manager.is_profiling();
    let continuous_sessions = state.profiling_manager.continuous_sessions().await;

    Ok((
        StatusCode::OK,
        Json(json!({
            "is_profiling": is_profiling,
            "current_session": session,
            "continuous_sessions": continuous_sessions
        })),
    ))
}

/// HTTP handler to stop profiling
pub async fn stop_profiling(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    match state.profiling_manager.stop_profiling().await {
        Ok(_) => Ok((
            StatusCode::OK,
            Json(json!({
                "message": "Profiling stopped successfully"
            })),
        )),
        Err(e) => {
            tracing::error!("Failed to stop profiling: {}", e);
            Err(AppError::BadRequest(e))
        }
    }
}

/// Session IDs are minted exclusively by `start_cpu_profiling`/
/// `start_memory_profiling`/`start_continuous_profiling` as
/// `profile-{cpu|memory|continuous}-{millis}` (see above) — never
/// client-supplied at creation time. Any request whose `session_id` doesn't
/// match this exact shape did not come from a real session and is rejected
/// before it ever reaches a path join, closing the path-traversal vector
/// regardless of what the authorization check below decides.
fn validate_session_id(session_id: &str) -> Result<(), AppError> {
    static SESSION_ID_RE: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"^profile-(cpu|memory|continuous)-[0-9]{1,20}$").unwrap()
    });

    if SESSION_ID_RE.is_match(session_id) {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!(
            "Invalid session_id '{session_id}': must match profile-(cpu|memory|continuous)-<millis>"
        )))
    }
}

/// Fail-closed authorization for flamegraph access.
///
/// A flamegraph is servable only if `session_id` exactly matches either the
/// single current on-demand/in-progress-rotation session, or one of the
/// retained completed continuous-profiling rotations — and in both cases,
/// only once that session has actually produced a flamegraph. Every other
/// case — no matching session at all, or a match that hasn't produced a
/// flamegraph yet — is denied.
///
/// This replaces a prior version of this function (removed, then never
/// restored, by an unrelated large revert) that inverted this logic: it
/// returned `Err` only for the single narrow "same session, still running"
/// case and `Ok(())` for every other input, including a `session_id` that
/// matched no known session at all — i.e. it failed *open* for the common
/// case of an attacker probing arbitrary IDs.
fn ensure_flamegraph_path_available(
    current_session: Option<&ProfilingSession>,
    continuous_sessions: &[ProfilingSession],
    session_id: &str,
) -> Result<(), AppError> {
    let matching = current_session
        .into_iter()
        .chain(continuous_sessions.iter())
        .find(|session| session.session_id == session_id);

    match matching {
        Some(session) => {
            if session.flamegraph_path.is_some() {
                Ok(())
            } else {
                Err(AppError::BadRequest(format!(
                    "Profiling session '{session_id}' has no flamegraph yet (status: {})",
                    session.status
                )))
            }
        }
        None => Err(AppError::NotFound(format!(
            "No accessible flamegraph for session '{session_id}'"
        ))),
    }
}

/// HTTP handler to serve a flamegraph SVG
pub async fn get_flamegraph(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    validate_session_id(&session_id)?;

    let current_session = state.profiling_manager.get_current_session().await;
    let continuous_sessions = state.profiling_manager.continuous_sessions().await;
    ensure_flamegraph_path_available(current_session.as_ref(), &continuous_sessions, &session_id)?;

    let profile_dir = PathBuf::from("./profiling_data");
    let flamegraph_path = profile_dir.join(format!("{session_id}.svg"));

    match tokio::fs::read_to_string(&flamegraph_path).await {
        Ok(content) => Ok((
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "image/svg+xml")],
            content,
        )),
        Err(_) => Err(AppError::NotFound(format!(
            "Flamegraph '{}' not found",
            session_id
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profiling_manager_creation() {
        let manager = ProfilingManager::new();
        assert!(!manager.is_profiling());
    }

    #[test]
    fn test_default_profiling_config() {
        let _req = StartProfilingRequest {
            duration_secs: 0,
            profile_type: "".to_string(),
            generate_flamegraph: false,
            sample_rate: None,
        };
        // Should compile with defaults
        assert_eq!(default_duration(), 30);
        assert_eq!(default_profile_type(), "cpu");
        assert!(default_generate_flamegraph());
    }

    #[tokio::test]
    async fn test_profiling_status_when_idle() {
        let manager = ProfilingManager::new();
        assert!(!manager.is_profiling());
        assert!(manager.get_current_session().await.is_none());
    }

    // ── validate_session_id: path-traversal regression ─────────────────────

    #[test]
    fn test_validate_session_id_accepts_real_shapes() {
        assert!(validate_session_id("profile-cpu-1735689600000").is_ok());
        assert!(validate_session_id("profile-memory-1735689600000").is_ok());
    }

    #[test]
    fn test_validate_session_id_rejects_path_traversal() {
        assert!(validate_session_id("../../etc/passwd").is_err());
        assert!(validate_session_id("profile-cpu-../../etc/passwd").is_err());
        assert!(validate_session_id("profile-cpu-1/../../secret").is_err());
        assert!(validate_session_id("profile-cpu-1%2e%2e%2f").is_err());
    }

    #[test]
    fn test_validate_session_id_rejects_wrong_shape() {
        assert!(validate_session_id("").is_err());
        assert!(validate_session_id("profile-disk-123").is_err());
        assert!(validate_session_id("profile-cpu-").is_err());
        assert!(validate_session_id("profile-cpu-12abc").is_err());
        assert!(validate_session_id("PROFILE-CPU-123").is_err());
    }

    // ── ensure_flamegraph_path_available: authorization-bypass regression ──

    fn completed_session(id: &str) -> ProfilingSession {
        ProfilingSession {
            session_id: id.to_string(),
            start_time: 0,
            end_time: Some(1),
            duration_secs: 1,
            profile_type: "cpu".to_string(),
            status: "completed".to_string(),
            flamegraph_path: Some(format!("./profiling_data/{id}.svg")),
            data_size_bytes: Some(100),
        }
    }

    #[test]
    fn test_ensure_flamegraph_path_available_allows_matching_completed_session() {
        let session = completed_session("profile-cpu-1");
        assert!(ensure_flamegraph_path_available(Some(&session), "profile-cpu-1").is_ok());
    }

    #[test]
    fn test_ensure_flamegraph_path_available_denies_no_current_session() {
        // The exact bypass this test guards against: an attacker probing an
        // arbitrary session_id when nothing is running must be denied, not
        // silently allowed through to the filesystem read.
        assert!(ensure_flamegraph_path_available(None, "profile-cpu-999").is_err());
    }

    #[test]
    fn test_ensure_flamegraph_path_available_denies_mismatched_session_id() {
        let session = completed_session("profile-cpu-1");
        assert!(ensure_flamegraph_path_available(Some(&session), "profile-cpu-2").is_err());
    }

    #[test]
    fn test_ensure_flamegraph_path_available_denies_session_without_flamegraph_yet() {
        let mut session = completed_session("profile-cpu-1");
        session.flamegraph_path = None;
        session.status = "running".to_string();
        assert!(ensure_flamegraph_path_available(Some(&session), "profile-cpu-1").is_err());
    }
}
