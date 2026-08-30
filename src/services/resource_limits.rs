//! Resource limits for background tasks.
//!
//! Provides semaphore-based concurrency control and timeout management
//! for background tasks to prevent resource exhaustion.

use std::sync::{Arc, Mutex, OnceLock, Weak};
use tokio::sync::Semaphore;
use tokio::time::{timeout, Duration};
use tracing::error;

/// Global registry of live [`ResourceLimiter`]s, keyed by resource category
/// (the limiter's `task_name`), used to expose active/limit metrics for all
/// categories without a caller having to hold onto every limiter instance.
/// Holds only a [`Weak`] reference to each limiter's semaphore so a dropped
/// limiter is not kept alive by the registry and is skipped on the next
/// snapshot instead of leaking.
static REGISTRY: OnceLock<Mutex<Vec<RegisteredLimiter>>> = OnceLock::new();

struct RegisteredLimiter {
    category: String,
    semaphore: Weak<Semaphore>,
    max_concurrent: usize,
}

fn registry() -> &'static Mutex<Vec<RegisteredLimiter>> {
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// Current active-task count and configured limit for one resource category,
/// as of the moment the snapshot was taken.
#[derive(Debug, Clone)]
pub struct ResourceCategorySnapshot {
    pub category: String,
    pub active: usize,
    pub limit: usize,
}

/// Snapshots active/limit for every currently-live registered resource
/// category. Reads each semaphore's atomic permit counter directly (no lock
/// held on the hot `run`/`active_tasks` path) — only the registry's own list
/// lock is taken here, on the metrics-emission path, not on task execution.
pub fn resource_category_snapshots() -> Vec<ResourceCategorySnapshot> {
    let mut reg = registry().lock().unwrap();
    reg.retain(|entry| entry.semaphore.strong_count() > 0);
    reg.iter()
        .filter_map(|entry| {
            entry.semaphore.upgrade().map(|sem| ResourceCategorySnapshot {
                category: entry.category.clone(),
                active: entry.max_concurrent - sem.available_permits(),
                limit: entry.max_concurrent,
            })
        })
        .collect()
}

/// Configuration for background task resource limits.
#[derive(Debug, Clone)]
pub struct TaskLimits {
    pub max_concurrent: usize,
    pub timeout_secs: u64,
}

impl TaskLimits {
    pub fn new(max_concurrent: usize, timeout_secs: u64) -> Self {
        Self {
            max_concurrent,
            timeout_secs,
        }
    }
}

/// Resource limiter for background tasks.
#[derive(Clone)]
pub struct ResourceLimiter {
    semaphore: Arc<Semaphore>,
    max_concurrent: usize,
    timeout_duration: Duration,
    task_name: String,
}

impl ResourceLimiter {
    pub fn new(limits: TaskLimits, task_name: impl Into<String>) -> Self {
        let semaphore = Arc::new(Semaphore::new(limits.max_concurrent));
        let task_name = task_name.into();

        registry().lock().unwrap().push(RegisteredLimiter {
            category: task_name.clone(),
            semaphore: Arc::downgrade(&semaphore),
            max_concurrent: limits.max_concurrent,
        });

        Self {
            semaphore,
            max_concurrent: limits.max_concurrent,
            timeout_duration: Duration::from_secs(limits.timeout_secs),
            task_name,
        }
    }

    /// Acquire a permit and run the future with timeout.
    /// Returns Ok(result) on success, Err on timeout or semaphore error.
    pub async fn run<F, T>(&self, future: F) -> Result<T, ResourceLimitError>
    where
        F: std::future::Future<Output = T>,
    {
        let permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| ResourceLimitError::SemaphoreError)?;

        let result = timeout(self.timeout_duration, future).await.map_err(|_| {
            crate::metrics::background_task_timeout_total().add(1, &[]);
            error!(
                task = %self.task_name,
                timeout_secs = self.timeout_duration.as_secs(),
                "Background task exceeded timeout"
            );
            ResourceLimitError::Timeout
        })?;

        drop(permit);
        Ok(result)
    }

    /// Number of permits currently held by in-flight tasks (i.e. tasks
    /// actually running right now), not the number still free. Was
    /// previously `self.semaphore.available_permits()` — the inverse of
    /// what the name and every caller of this metric expects (see issue
    /// tracking #961): idle would have read as fully saturated, and vice
    /// versa. No live call site read this before the fix (confirmed by
    /// grep), so nothing was silently misreporting in production, but any
    /// observability wired to it afterward would have been.
    pub fn active_tasks(&self) -> usize {
        self.max_concurrent - self.semaphore.available_permits()
    }

    /// Number of permits still free (i.e. additional tasks that could start
    /// immediately without waiting). This is [`Semaphore::available_permits`]
    /// directly — the value [`Self::active_tasks`] used to incorrectly
    /// return under its own name.
    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }
}

#[derive(Debug)]
pub enum ResourceLimitError {
    Timeout,
    SemaphoreError,
}

impl std::fmt::Display for ResourceLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => write!(f, "Task execution timeout"),
            Self::SemaphoreError => write!(f, "Failed to acquire semaphore permit"),
        }
    }
}

impl std::error::Error for ResourceLimitError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_semaphore_limits_concurrency() {
        let limits = TaskLimits::new(2, 10);
        let limiter = ResourceLimiter::new(limits, "test");

        let mut handles = vec![];
        for _ in 0..3 {
            let limiter = limiter.clone();
            handles.push(tokio::spawn(async move {
                limiter
                    .run(async {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    })
                    .await
            }));
        }

        for handle in handles {
            let _ = handle.await;
        }
    }

    #[tokio::test]
    async fn test_timeout_cancels_task() {
        let limits = TaskLimits::new(1, 1);
        let limiter = ResourceLimiter::new(limits, "test");

        let result = limiter
            .run(async {
                tokio::time::sleep(Duration::from_secs(5)).await;
            })
            .await;

        assert!(matches!(result, Err(ResourceLimitError::Timeout)));
    }
}
