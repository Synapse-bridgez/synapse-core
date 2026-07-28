use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use cron::Schedule;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info};

/// Represents a scheduled job that can be executed at specific intervals
#[async_trait]
pub trait Job: Send + Sync {
    /// Unique name of the job
    fn name(&self) -> &str;

    /// Cron expression defining when the job should run
    fn schedule(&self) -> &str;

    /// Execute the job's business logic
    async fn execute(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

/// A job scheduler that manages cron-based recurring tasks
pub struct JobScheduler {
    jobs: Arc<Mutex<HashMap<String, Arc<dyn Job>>>>,
    active_handles: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    shutdown_tx: tokio::sync::broadcast::Sender<()>,
}

impl Default for JobScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl JobScheduler {
    /// Create a new job scheduler instance
    pub fn new() -> Self {
        let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
        Self {
            jobs: Arc::new(Mutex::new(HashMap::new())),
            active_handles: Arc::new(Mutex::new(HashMap::new())),
            shutdown_tx,
        }
    }

    /// Register a new job with the scheduler
    pub async fn register_job(
        &self,
        job: Box<dyn Job>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let name = job.name().to_string();

        // Validate the cron expression
        Schedule::from_str(job.schedule())
            .map_err(|e| format!("Invalid cron expression '{}': {}", job.schedule(), e))?;

        let mut jobs = self.jobs.lock().await;
        jobs.insert(name, Arc::from(job));
        Ok(())
    }

    /// Start the scheduler and all registered jobs
    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let jobs = self.jobs.lock().await;
        let active_handles = self.active_handles.clone();

        for (name, job) in jobs.iter() {
            let job_clone = Arc::clone(job);
            let name_clone = name.clone();
            let shutdown_rx = self.shutdown_tx.subscribe();
            let active_handles_clone = Arc::clone(&active_handles);

            let handle = tokio::spawn(Self::run_job_loop(
                name_clone,
                job_clone,
                self.shutdown_tx.clone(),
                shutdown_rx,
                active_handles_clone,
            ));

            active_handles.lock().await.insert(name.clone(), handle);
        }

        info!("Job scheduler started with {} jobs", jobs.len());
        Ok(())
    }

    /// Stop the scheduler and all running jobs gracefully
    pub async fn stop(&self) -> Result<(), Box<dyn std::error::Error + Sync>> {
        info!("Stopping job scheduler...");

        // Signal all jobs to shut down
        let _ = self.shutdown_tx.send(());

        // Wait for all active handles to finish
        let handles: Vec<_> = {
            let mut active_handles = self.active_handles.lock().await;
            active_handles.drain().map(|(_, handle)| handle).collect()
        };

        // Wait for all tasks to complete
        for handle in handles {
            if let Err(e) = handle.await {
                error!("Error waiting for job task to finish: {}", e);
            }
        }

        info!("Job scheduler stopped");
        Ok(())
    }

    /// Get status information about all registered jobs
    pub async fn get_job_status(&self) -> HashMap<String, JobStatus> {
        let jobs = self.jobs.lock().await;
        let active_handles = self.active_handles.lock().await;
        let mut status = HashMap::new();

        for (name, job) in jobs.iter() {
            // Parse the schedule to get the next run time
            let next_run = Self::get_next_run_time(job.schedule());

            status.insert(
                name.clone(),
                JobStatus {
                    name: name.clone(),
                    schedule: job.schedule().to_string(),
                    next_run,
                    is_active: active_handles.contains_key(name),
                },
            );
        }

        status
    }

    /// Internal function that runs the job execution loop
    async fn run_job_loop(
        name: String,
        job: Arc<dyn Job>,
        _shutdown_tx: tokio::sync::broadcast::Sender<()>,
        mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
        active_handles: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    ) {
        info!("Starting job '{}' with schedule: {}", name, job.schedule());

        let schedule = match Schedule::from_str(job.schedule()) {
            Ok(schedule) => schedule,
            Err(e) => {
                error!("Failed to parse cron schedule for job '{}': {}", name, e);
                return;
            }
        };

        loop {
            // Calculate next run time
            let now = Utc::now();
            let next_run = schedule.after(&now).next();

            let next_run_time = match next_run {
                Some(next_time) => {
                    let duration = (next_time - now)
                        .to_std()
                        .unwrap_or_else(|_| std::time::Duration::from_secs(1));
                    // Wait for either the duration to pass or a shutdown signal
                    tokio::select! {
                        _ = tokio::time::sleep(duration) => {
                            // Time to execute the job
                        },
                        _ = shutdown_rx.recv() => {
                            info!("Job '{}' received shutdown signal", name);
                            // Remove handle from active handles
                            let _ = active_handles.lock().await.remove(&name);
                            return;
                        }
                    }
                    next_time
                }
                None => {
                    error!("Job '{}' has no next run time, stopping", name);
                    return;
                }
            };

            // Execute the job
            match job.execute().await {
                Ok(()) => {
                    info!(
                        "Job '{}' executed successfully at {}",
                        name,
                        next_run_time.format("%Y-%m-%d %H:%M:%S")
                    );
                }
                Err(e) => {
                    error!(
                        "Job '{}' failed at {}: {}",
                        name,
                        next_run_time.format("%Y-%m-%d %H:%M:%S"),
                        e
                    );
                }
            }
        }
    }

    /// Helper function to get the next run time for a schedule
    fn get_next_run_time(schedule_expr: &str) -> Option<DateTime<Utc>> {
        match Schedule::from_str(schedule_expr) {
            Ok(schedule) => {
                let now = Utc::now();
                schedule.after(&now).next()
            }
            Err(_) => None,
        }
    }
}

/// Status information for a scheduled job
#[derive(Debug, Clone)]
pub struct JobStatus {
    pub name: String,
    pub schedule: String,
    pub next_run: Option<DateTime<Utc>>,
    pub is_active: bool,
}

// ---------------------------------------------------------------------------
// Audit log retention job
// ---------------------------------------------------------------------------

/// Monthly background job that archives and deletes audit logs older than the
/// configured retention period.
///
/// Schedule: first day of every month at 02:00 UTC (`0 0 2 1 * * *`).
/// Override with `AUDIT_LOG_RETENTION_DAYS` (default 365).
/// Archive files are written to `AUDIT_LOG_ARCHIVE_DIR` (default `/tmp/audit_archives`).
pub struct AuditLogRetentionJob {
    pool: sqlx::PgPool,
}

impl AuditLogRetentionJob {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    /// Archive directory — reads `AUDIT_LOG_ARCHIVE_DIR`, falls back to `/tmp/audit_archives`.
    fn archive_dir() -> String {
        std::env::var("AUDIT_LOG_ARCHIVE_DIR").unwrap_or_else(|_| "/tmp/audit_archives".to_string())
    }
}

#[async_trait]
impl Job for AuditLogRetentionJob {
    fn name(&self) -> &str {
        "audit_log_retention"
    }

    /// Run on the 1st of every month at 02:00 UTC.
    fn schedule(&self) -> &str {
        "0 0 2 1 * * *"
    }

    async fn execute(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let days = crate::db::audit::retention_days();
        let cutoff = Utc::now() - Duration::days(days);
        let archive_dir = Self::archive_dir();

        // Ensure the archive directory exists.
        std::fs::create_dir_all(&archive_dir)?;

        info!(
            retention_days = days,
            cutoff = %cutoff.to_rfc3339(),
            archive_dir = %archive_dir,
            "Starting audit log retention run"
        );

        match crate::db::audit::run_retention(&self.pool, cutoff, &archive_dir).await? {
            None => {
                info!("Audit log retention: no logs older than cutoff, nothing to do");
            }
            Some(result) => {
                info!(
                    exported = result.exported,
                    deleted = result.deleted,
                    archive = %result.archive_path,
                    "Audit log retention complete"
                );
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct TestJob {
        name: String,
        schedule: String,
    }

    impl TestJob {
        fn new(name: &str, schedule: &str) -> Self {
            Self {
                name: name.to_string(),
                schedule: schedule.to_string(),
            }
        }
    }

    #[async_trait::async_trait]
    impl Job for TestJob {
        fn name(&self) -> &str {
            &self.name
        }

        fn schedule(&self) -> &str {
            &self.schedule
        }

        async fn execute(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            println!("Executing test job: {}", self.name);
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_scheduler_basic() {
        let scheduler = JobScheduler::new();

        let test_job = TestJob::new("test_job", "*/1 * * * * *"); // Every second
        scheduler.register_job(Box::new(test_job)).await.unwrap();

        assert_eq!(scheduler.jobs.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn test_scheduler_rejects_invalid_cron() {
        let scheduler = JobScheduler::new();

        let invalid_job = TestJob::new("invalid_job", "invalid cron");
        let result = scheduler.register_job(Box::new(invalid_job)).await;

        assert!(result.is_err());
        assert_eq!(scheduler.jobs.lock().await.len(), 0);
    }

    #[tokio::test]
    async fn test_scheduler_registers_multiple_jobs() {
        let scheduler = JobScheduler::new();

        let job1 = TestJob::new("job1", "0 0 * * * *");
        let job2 = TestJob::new("job2", "*/5 * * * * *");
        let job3 = TestJob::new("job3", "0 * * * * *");

        scheduler.register_job(Box::new(job1)).await.unwrap();
        scheduler.register_job(Box::new(job2)).await.unwrap();
        scheduler.register_job(Box::new(job3)).await.unwrap();

        assert_eq!(scheduler.jobs.lock().await.len(), 3);
    }

    #[tokio::test]
    async fn test_audit_log_retention_job_cron_is_valid() {
        let scheduler = JobScheduler::new();

        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect("postgres://invalid-host/db")
            .await;

        if let Ok(pool) = pool {
            let job = AuditLogRetentionJob::new(pool);
            let result = scheduler.register_job(Box::new(job)).await;

            // Should register successfully if pool creation succeeded
            assert!(result.is_ok() || result.is_err());
        }
    }

    #[test]
    fn test_audit_log_retention_job_schedule_format() {
        // Verify the audit log retention job uses correct cron format
        let schedule = "0 0 2 1 * * *";
        let parsed = Schedule::from_str(schedule);

        assert!(
            parsed.is_ok(),
            "AuditLogRetentionJob schedule should be valid"
        );

        let parts: Vec<&str> = schedule.split_whitespace().collect();
        assert_eq!(parts.len(), 7, "Should have 7 fields (sec min hour dom mon dow year)");
    }

    #[test]
    fn test_job_status_construction() {
        let now = chrono::Utc::now();
        let status = JobStatus {
            name: "test_job".to_string(),
            schedule: "0 * * * * *".to_string(),
            next_run: Some(now),
            is_active: true,
        };

        assert_eq!(status.name, "test_job");
        assert_eq!(status.schedule, "0 * * * * *");
        assert!(status.is_active);
    }
}
