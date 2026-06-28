//! Resource limits for background tasks.
//!
//! Provides semaphore-based concurrency control and timeout management
//! for background tasks to prevent resource exhaustion.

use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::time::{timeout, Duration};
use tracing::error;

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
    timeout_duration: Duration,
    task_name: String,
}

impl ResourceLimiter {
    pub fn new(limits: TaskLimits, task_name: impl Into<String>) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(limits.max_concurrent)),
            timeout_duration: Duration::from_secs(limits.timeout_secs),
            task_name: task_name.into(),
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

    /// Get current number of active tasks.
    pub fn active_tasks(&self) -> usize {
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
