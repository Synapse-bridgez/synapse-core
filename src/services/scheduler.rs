use chrono::{DateTime, Utc};
use cron::Schedule;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info};
use async_trait::async_trait;

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
    pub async fn register_job(&self, job: Box<dyn Job>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
                    let duration = (next_time - now).to_std().unwrap_or_else(|_| std::time::Duration::from_secs(1));
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
                    info!("Job '{}' executed successfully at {}", name, next_run_time.format("%Y-%m-%d %H:%M:%S"));
                }
                Err(e) => {
                    error!("Job '{}' failed at {}: {}", name, next_run_time.format("%Y-%m-%d %H:%M:%S"), e);
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{timeout, Duration};

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
}