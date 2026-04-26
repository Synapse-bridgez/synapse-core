/// Tests for #271 — Tenant-Specific Rate Limiting and Quota Management
///
/// Validates:
/// - Two tenants have independent quota buckets
/// - Exhausting one tenant's quota does not affect the other
/// - Per-tenant custom limits override tier defaults
use synapse_core::middleware::quota::{Quota, QuotaManager, ResetSchedule, Tier};

fn make_manager() -> QuotaManager {
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    QuotaManager::new(&redis_url).expect("QuotaManager::new")
}

/// Unique key prefix per test run to avoid cross-test pollution.
fn key(tenant: &str) -> String {
    format!("test:quota:{}:{}", tenant, uuid::Uuid::new_v4())
}

#[tokio::test]
#[ignore = "Requires Redis"]
async fn test_two_tenants_have_independent_quotas() {
    let mgr = make_manager();
    let k1 = key("tenant_a");
    let k2 = key("tenant_b");

    // Tenant A: limit 2, Tenant B: limit 5
    let q1 = Quota { tier: Tier::Free, custom_limit: Some(2), reset_schedule: ResetSchedule::Hourly };
    let q2 = Quota { tier: Tier::Free, custom_limit: Some(5), reset_schedule: ResetSchedule::Hourly };
    mgr.set_quota_config(&k1, &q1).await.unwrap();
    mgr.set_quota_config(&k2, &q2).await.unwrap();

    // Exhaust tenant A (2 requests)
    assert!(mgr.consume_quota_with_window(&k1, 2, 60).await.unwrap()); // 1
    assert!(mgr.consume_quota_with_window(&k1, 2, 60).await.unwrap()); // 2
    let a_blocked = !mgr.consume_quota_with_window(&k1, 2, 60).await.unwrap(); // 3 → over limit
    assert!(a_blocked, "tenant A should be rate-limited after 2 requests");

    // Tenant B should still have capacity
    let b_allowed = mgr.consume_quota_with_window(&k2, 5, 60).await.unwrap();
    assert!(b_allowed, "tenant B should not be affected by tenant A's exhaustion");
}

#[tokio::test]
#[ignore = "Requires Redis"]
async fn test_exceeding_one_tenant_quota_does_not_affect_other() {
    let mgr = make_manager();
    let k1 = key("noisy_tenant");
    let k2 = key("quiet_tenant");

    // Noisy tenant: limit 1
    for _ in 0..10 {
        let _ = mgr.consume_quota_with_window(&k1, 1, 60).await;
    }

    // Quiet tenant: limit 100 — should be completely unaffected
    let status = mgr.check_quota_with_limit(&k2, 100).await.unwrap();
    assert_eq!(status.used, 0, "quiet tenant usage should be 0");
    assert_eq!(status.remaining, 100);
}

#[tokio::test]
#[ignore = "Requires Redis"]
async fn test_custom_limit_overrides_tier_default() {
    let mgr = make_manager();
    let k = key("custom_limit_tenant");

    // Free tier default is 100 req/hour; override to 3
    let quota = Quota { tier: Tier::Free, custom_limit: Some(3), reset_schedule: ResetSchedule::Hourly };
    mgr.set_quota_config(&k, &quota).await.unwrap();

    let loaded = mgr.get_quota_config(&k).await.unwrap();
    let effective_limit = loaded.custom_limit.unwrap_or_else(|| loaded.tier.requests_per_hour());
    assert_eq!(effective_limit, 3, "custom_limit should override tier default");
}

#[tokio::test]
#[ignore = "Requires Redis"]
async fn test_quota_reset_clears_usage() {
    let mgr = make_manager();
    let k = key("reset_tenant");

    mgr.consume_quota_with_window(&k, 10, 60).await.unwrap();
    mgr.consume_quota_with_window(&k, 10, 60).await.unwrap();

    let before = mgr.check_quota_with_limit(&k, 10).await.unwrap();
    assert_eq!(before.used, 2);

    mgr.reset_quota(&k).await.unwrap();

    let after = mgr.check_quota_with_limit(&k, 10).await.unwrap();
    assert_eq!(after.used, 0, "usage should be 0 after reset");
}
