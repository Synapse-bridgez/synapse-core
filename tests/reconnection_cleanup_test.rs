use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, Clone)]
struct ConnectionState {
    session_id: Uuid,
    last_sequence: i64,
    last_connected: i64,
    reconnect_attempts: u32,
    created_at: std::time::Instant,
}

impl ConnectionState {
    fn new() -> Self {
        Self {
            session_id: Uuid::new_v4(),
            last_sequence: 0,
            last_connected: chrono::Utc::now().timestamp(),
            reconnect_attempts: 0,
            created_at: std::time::Instant::now(),
        }
    }
}

async fn cleanup_stale_sessions(
    store: Arc<Mutex<std::collections::HashMap<Uuid, ConnectionState>>>,
) {
    let mut store = store.lock().await;
    let now = std::time::Instant::now();

    store.retain(|_, state| now.duration_since(state.created_at).as_secs() < 3600);
}

#[tokio::test]
async fn test_cleanup_stale_sessions_removes_old_sessions() {
    let store = Arc::new(Mutex::new(
        std::collections::HashMap::<Uuid, ConnectionState>::new(),
    ));

    let mut session = ConnectionState::new();
    session.created_at = std::time::Instant::now() - Duration::from_secs(3700);

    let session_id = session.session_id;

    store.lock().await.insert(session_id, session);

    assert_eq!(
        store.lock().await.len(),
        1,
        "Session should exist before cleanup"
    );

    cleanup_stale_sessions(store.clone()).await;

    assert_eq!(
        store.lock().await.len(),
        0,
        "Old session should be removed after cleanup"
    );
}

#[tokio::test]
async fn test_cleanup_stale_sessions_keeps_recent_sessions() {
    let store = Arc::new(Mutex::new(
        std::collections::HashMap::<Uuid, ConnectionState>::new(),
    ));

    let session = ConnectionState::new();
    let session_id = session.session_id;

    store.lock().await.insert(session_id, session);

    assert_eq!(
        store.lock().await.len(),
        1,
        "Session should exist before cleanup"
    );

    cleanup_stale_sessions(store.clone()).await;

    assert_eq!(
        store.lock().await.len(),
        1,
        "Recent session should be kept after cleanup"
    );
}

#[tokio::test]
async fn test_cleanup_stale_sessions_mixed_ages() {
    let store = Arc::new(Mutex::new(
        std::collections::HashMap::<Uuid, ConnectionState>::new(),
    ));

    let mut old_session = ConnectionState::new();
    old_session.created_at = std::time::Instant::now() - Duration::from_secs(3700);
    let old_id = old_session.session_id;

    let new_session = ConnectionState::new();
    let new_id = new_session.session_id;

    store.lock().await.insert(old_id, old_session);
    store.lock().await.insert(new_id, new_session);

    assert_eq!(
        store.lock().await.len(),
        2,
        "Both sessions should exist before cleanup"
    );

    cleanup_stale_sessions(store.clone()).await;

    let remaining = store.lock().await;
    assert_eq!(
        remaining.len(),
        1,
        "Only recent session should remain after cleanup"
    );
    assert!(
        remaining.contains_key(&new_id),
        "Recent session should be the one that remains"
    );
    assert!(
        !remaining.contains_key(&old_id),
        "Old session should be removed"
    );
}

#[tokio::test]
async fn test_cleanup_stale_sessions_boundary_case() {
    let store = Arc::new(Mutex::new(
        std::collections::HashMap::<Uuid, ConnectionState>::new(),
    ));

    let mut boundary_session = ConnectionState::new();
    boundary_session.created_at = std::time::Instant::now() - Duration::from_secs(3599);
    let boundary_id = boundary_session.session_id;

    store.lock().await.insert(boundary_id, boundary_session);

    assert_eq!(
        store.lock().await.len(),
        1,
        "Session should exist before cleanup"
    );

    cleanup_stale_sessions(store.clone()).await;

    assert_eq!(
        store.lock().await.len(),
        1,
        "Session just under 1 hour should be kept"
    );
}
