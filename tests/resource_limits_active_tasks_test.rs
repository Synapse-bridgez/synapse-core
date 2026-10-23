/// Tests for #961 — ResourceLimiter::active_tasks() returns inverted semantics
///
/// Validates that active_tasks() correctly returns the number of tasks
/// currently executing (permits in use), not the number of available permits.
use synapse_core::services::resource_limits::{ResourceLimiter, TaskLimits};
use tokio::time::Duration;

#[tokio::test]
async fn test_active_tasks_returns_zero_when_idle() {
    let limits = TaskLimits::new(5, 10);
    let limiter = ResourceLimiter::new(limits, "test");

    // When idle, no tasks should be active
    assert_eq!(
        limiter.active_tasks(),
        0,
        "active_tasks() should return 0 when limiter is idle"
    );
}

#[tokio::test]
async fn test_active_tasks_returns_max_when_saturated() {
    let limits = TaskLimits::new(3, 10);
    let limiter = ResourceLimiter::new(limits, "test");

    // Spawn 3 concurrent tasks to saturate the limiter
    let mut handles = vec![];
    for _ in 0..3 {
        let limiter_clone = limiter.clone();
        handles.push(tokio::spawn(async move {
            limiter_clone
                .run(async {
                    // Hold the permit for a bit
                    tokio::time::sleep(Duration::from_millis(100)).await;
                })
                .await
        }));
    }

    // Give tasks time to acquire permits
    tokio::time::sleep(Duration::from_millis(10)).await;

    // When saturated, all 3 permits should be in use
    assert_eq!(
        limiter.active_tasks(),
        3,
        "active_tasks() should return 3 when all permits are in use"
    );

    // Wait for tasks to complete
    for handle in handles {
        let _ = handle.await;
    }
}

#[tokio::test]
async fn test_active_tasks_reflects_current_load() {
    let limits = TaskLimits::new(5, 10);
    let limiter = ResourceLimiter::new(limits, "test");

    // Start 2 concurrent tasks
    let mut handles = vec![];
    for _ in 0..2 {
        let limiter_clone = limiter.clone();
        handles.push(tokio::spawn(async move {
            limiter_clone
                .run(async {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                })
                .await
        }));
    }

    // Give tasks time to acquire permits
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Should show 2 active tasks
    assert_eq!(
        limiter.active_tasks(),
        2,
        "active_tasks() should return 2 when 2 permits are in use"
    );

    // Start one more task
    let limiter_clone = limiter.clone();
    handles.push(tokio::spawn(async move {
        limiter_clone
            .run(async {
                tokio::time::sleep(Duration::from_millis(100)).await;
            })
            .await
    }));

    tokio::time::sleep(Duration::from_millis(10)).await;

    // Should now show 3 active tasks
    assert_eq!(
        limiter.active_tasks(),
        3,
        "active_tasks() should return 3 when 3 permits are in use"
    );

    // Wait for all tasks to complete
    for handle in handles {
        let _ = handle.await;
    }
}

#[tokio::test]
async fn test_active_tasks_returns_correct_count_after_release() {
    let limits = TaskLimits::new(4, 10);
    let limiter = ResourceLimiter::new(limits, "test");

    // Start 2 tasks that complete quickly
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let limiter_clone = limiter.clone();
            tokio::spawn(async move {
                limiter_clone
                    .run(async {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    })
                    .await
            })
        })
        .collect();

    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(
        limiter.active_tasks(),
        2,
        "should show 2 active tasks initially"
    );

    // Wait for first 2 tasks to complete
    for handle in handles {
        let _ = handle.await;
    }

    tokio::time::sleep(Duration::from_millis(10)).await;

    // After tasks complete, should return to 0
    assert_eq!(
        limiter.active_tasks(),
        0,
        "active_tasks() should return 0 after permits are released"
    );
}

#[tokio::test]
async fn test_active_tasks_under_load() {
    let limits = TaskLimits::new(10, 10);
    let limiter = ResourceLimiter::new(limits, "test");

    // Start 7 concurrent tasks
    let handles: Vec<_> = (0..7)
        .map(|_| {
            let limiter_clone = limiter.clone();
            tokio::spawn(async move {
                limiter_clone
                    .run(async {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    })
                    .await
            })
        })
        .collect();

    tokio::time::sleep(Duration::from_millis(10)).await;

    // Should show exactly 7 active tasks
    assert_eq!(
        limiter.active_tasks(),
        7,
        "active_tasks() should return 7 when 7 permits are held out of 10"
    );

    // Wait for all to complete
    for handle in handles {
        let _ = handle.await;
    }

    tokio::time::sleep(Duration::from_millis(10)).await;

    // Back to 0
    assert_eq!(
        limiter.active_tasks(),
        0,
        "active_tasks() should return 0 when all tasks complete"
    );
}
