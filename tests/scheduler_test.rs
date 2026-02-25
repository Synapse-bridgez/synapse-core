use async_trait::async_trait;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use synapse_core::services::scheduler::{Job, JobScheduler};
use tokio::time::{sleep, Duration};

// Test job that counts executions
#[derive(Clone)]
struct CounterJob {
    name: String,
    schedule: String,
    counter: Arc<AtomicU32>,
}

impl CounterJob {
    fn new(name: &str, schedule: &str, counter: Arc<AtomicU32>) -> Self {
        Self {
            name: name.to_string(),
            schedule: schedule.to_string(),
            counter,
        }
    }
}

#[async_trait]
impl Job for CounterJob {
    fn name(&self) -> &str {
        &self.name
    }

    fn schedule(&self) -> &str {
        &self.schedule
    }

    async fn execute(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.counter.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

// Test job that fails
#[derive(Clone)]
struct FailingJob {
    name: String,
    schedule: String,
    counter: Arc<AtomicU32>,
}

impl FailingJob {
    fn new(name: &str, schedule: &str, counter: Arc<AtomicU32>) -> Self {
        Self {
            name: name.to_string(),
            schedule: schedule.to_string(),
            counter,
        }
    }
}

#[async_trait]
impl Job for FailingJob {
    fn name(&self) -> &str {
        &self.name
    }

    fn schedule(&self) -> &str {
        &self.schedule
    }

    async fn execute(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.counter.fetch_add(1, Ordering::SeqCst);
        Err("Intentional failure".into())
    }
}

#[tokio::test]
async fn test_scheduler_job_execution() {
    let scheduler = JobScheduler::new();
    let counter = Arc::new(AtomicU32::new(0));

    // Register a job that runs every second
    let job = CounterJob::new("test_job", "*/1 * * * * *", counter.clone());
    scheduler.register_job(Box::new(job)).await.unwrap();

    // Start the scheduler
    scheduler.start().await.unwrap();

    // Wait for job to execute at least twice
    sleep(Duration::from_secs(3)).await;

    // Stop the scheduler
    scheduler.stop().await.unwrap();

    // Verify job executed at least twice
    let count = counter.load(Ordering::SeqCst);
    assert!(count >= 2, "Expected at least 2 executions, got {}", count);
}

#[tokio::test]
async fn test_scheduler_cron_scheduling() {
    let scheduler = JobScheduler::new();
    let counter = Arc::new(AtomicU32::new(0));

    // Register a job with a specific cron expression (every 2 seconds)
    let job = CounterJob::new("cron_job", "*/2 * * * * *", counter.clone());
    scheduler.register_job(Box::new(job)).await.unwrap();

    scheduler.start().await.unwrap();

    // Wait for 5 seconds
    sleep(Duration::from_secs(5)).await;

    scheduler.stop().await.unwrap();

    // Should execute 2-3 times in 5 seconds (at 0s, 2s, 4s)
    let count = counter.load(Ordering::SeqCst);
    assert!(
        (2..=3).contains(&count),
        "Expected 2-3 executions, got {}",
        count
    );
}

#[tokio::test]
async fn test_scheduler_job_error_handling() {
    let scheduler = JobScheduler::new();
    let counter = Arc::new(AtomicU32::new(0));

    // Register a job that always fails
    let job = FailingJob::new("failing_job", "*/1 * * * * *", counter.clone());
    scheduler.register_job(Box::new(job)).await.unwrap();

    scheduler.start().await.unwrap();

    // Wait for job to attempt execution multiple times
    sleep(Duration::from_secs(3)).await;

    scheduler.stop().await.unwrap();

    // Verify job continued to execute despite failures
    let count = counter.load(Ordering::SeqCst);
    assert!(
        count >= 2,
        "Expected at least 2 execution attempts, got {}",
        count
    );
}

#[tokio::test]
async fn test_scheduler_job_status() {
    let scheduler = JobScheduler::new();
    let counter1 = Arc::new(AtomicU32::new(0));
    let counter2 = Arc::new(AtomicU32::new(0));

    // Register multiple jobs
    let job1 = CounterJob::new("job1", "*/1 * * * * *", counter1);
    let job2 = CounterJob::new("job2", "*/2 * * * * *", counter2);

    scheduler.register_job(Box::new(job1)).await.unwrap();
    scheduler.register_job(Box::new(job2)).await.unwrap();

    // Check status before starting
    let status_before = scheduler.get_job_status().await;
    assert_eq!(status_before.len(), 2);
    assert!(status_before.contains_key("job1"));
    assert!(status_before.contains_key("job2"));
    assert!(!status_before.get("job1").unwrap().is_active);
    assert!(!status_before.get("job2").unwrap().is_active);

    // Start scheduler
    scheduler.start().await.unwrap();

    // Check status after starting
    let status_after = scheduler.get_job_status().await;
    assert_eq!(status_after.len(), 2);
    assert!(status_after.get("job1").unwrap().is_active);
    assert!(status_after.get("job2").unwrap().is_active);
    assert!(status_after.get("job1").unwrap().next_run.is_some());
    assert!(status_after.get("job2").unwrap().next_run.is_some());

    scheduler.stop().await.unwrap();
}

#[tokio::test]
async fn test_scheduler_shutdown() {
    let scheduler = JobScheduler::new();
    let counter = Arc::new(AtomicU32::new(0));

    // Register a job
    let job = CounterJob::new("shutdown_test", "*/1 * * * * *", counter.clone());
    scheduler.register_job(Box::new(job)).await.unwrap();

    scheduler.start().await.unwrap();

    // Let it run for a bit
    sleep(Duration::from_secs(2)).await;

    let count_before_stop = counter.load(Ordering::SeqCst);

    // Stop the scheduler
    scheduler.stop().await.unwrap();

    // Wait a bit more
    sleep(Duration::from_secs(2)).await;

    // Verify no more executions after shutdown
    let count_after_stop = counter.load(Ordering::SeqCst);
    assert_eq!(
        count_before_stop, count_after_stop,
        "Job should not execute after shutdown"
    );
}

#[tokio::test]
async fn test_scheduler_invalid_cron() {
    let scheduler = JobScheduler::new();
    let counter = Arc::new(AtomicU32::new(0));

    // Try to register a job with invalid cron expression
    let job = CounterJob::new("invalid_job", "invalid cron", counter);
    let result = scheduler.register_job(Box::new(job)).await;

    assert!(result.is_err(), "Should fail with invalid cron expression");
}

#[tokio::test]
async fn test_scheduler_multiple_jobs() {
    let scheduler = JobScheduler::new();
    let counter1 = Arc::new(AtomicU32::new(0));
    let counter2 = Arc::new(AtomicU32::new(0));
    let counter3 = Arc::new(AtomicU32::new(0));

    // Register multiple jobs with different schedules
    let job1 = CounterJob::new("fast_job", "*/1 * * * * *", counter1.clone());
    let job2 = CounterJob::new("medium_job", "*/2 * * * * *", counter2.clone());
    let job3 = CounterJob::new("slow_job", "*/3 * * * * *", counter3.clone());

    scheduler.register_job(Box::new(job1)).await.unwrap();
    scheduler.register_job(Box::new(job2)).await.unwrap();
    scheduler.register_job(Box::new(job3)).await.unwrap();

    scheduler.start().await.unwrap();

    // Wait for 7 seconds
    sleep(Duration::from_secs(7)).await;

    scheduler.stop().await.unwrap();

    // Verify each job executed according to its schedule
    let count1 = counter1.load(Ordering::SeqCst);
    let count2 = counter2.load(Ordering::SeqCst);
    let count3 = counter3.load(Ordering::SeqCst);

    assert!(
        count1 >= 5,
        "Fast job should execute ~5-7 times, got {}",
        count1
    );
    assert!(
        count2 >= 2,
        "Medium job should execute ~2-4 times, got {}",
        count2
    );
    assert!(
        count3 >= 1,
        "Slow job should execute ~1-3 times, got {}",
        count3
    );

    // Verify relative execution counts (with tolerance for timing)
    assert!(
        count1 >= count2,
        "Fast job should execute at least as many times as medium"
    );
    assert!(
        count2 >= count3,
        "Medium job should execute at least as many times as slow"
    );
}
