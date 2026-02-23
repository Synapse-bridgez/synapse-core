use synapse_core::health::*;
use std::time::Instant;

#[tokio::test]
async fn test_health_check_response_structure() {
    // This test verifies the response structure matches requirements
    // Note: Requires DATABASE_URL, REDIS_URL, and STELLAR_HORIZON_URL to be set
    
    // Mock test - verifying types compile correctly
    let start_time = Instant::now();
    
    // Verify DependencyStatus variants
    let healthy = DependencyStatus::Healthy {
        status: "healthy".to_string(),
        latency_ms: 10,
    };
    
    let unhealthy = DependencyStatus::Unhealthy {
        status: "unhealthy".to_string(),
        error: "connection refused".to_string(),
    };
    
    // Verify serialization works
    let healthy_json = serde_json::to_string(&healthy).unwrap();
    assert!(healthy_json.contains("healthy"));
    assert!(healthy_json.contains("latency_ms"));
    
    let unhealthy_json = serde_json::to_string(&unhealthy).unwrap();
    assert!(unhealthy_json.contains("unhealthy"));
    assert!(unhealthy_json.contains("error"));
}

#[test]
fn test_dependency_status_serialization() {
    // Test that DependencyStatus serializes to the correct JSON format
    let healthy = DependencyStatus::Healthy {
        status: "healthy".to_string(),
        latency_ms: 42,
    };
    
    let json = serde_json::to_value(&healthy).unwrap();
    assert_eq!(json["status"], "healthy");
    assert_eq!(json["latency_ms"], 42);
    assert!(json.get("error").is_none());
    
    let unhealthy = DependencyStatus::Unhealthy {
        status: "unhealthy".to_string(),
        error: "timeout".to_string(),
    };
    
    let json = serde_json::to_value(&unhealthy).unwrap();
    assert_eq!(json["status"], "unhealthy");
    assert_eq!(json["error"], "timeout");
    assert!(json.get("latency_ms").is_none());
}

#[test]
fn test_health_response_structure() {
    use std::collections::HashMap;
    
    let mut dependencies = HashMap::new();
    dependencies.insert(
        "postgres".to_string(),
        DependencyStatus::Healthy {
            status: "healthy".to_string(),
            latency_ms: 5,
        },
    );
    dependencies.insert(
        "redis".to_string(),
        DependencyStatus::Unhealthy {
            status: "unhealthy".to_string(),
            error: "connection refused".to_string(),
        },
    );
    dependencies.insert(
        "horizon".to_string(),
        DependencyStatus::Healthy {
            status: "healthy".to_string(),
            latency_ms: 150,
        },
    );
    
    let response = HealthResponse {
        status: "degraded".to_string(),
        version: "0.1.0".to_string(),
        uptime_seconds: 3600,
        dependencies,
    };
    
    let json = serde_json::to_string_pretty(&response).unwrap();
    println!("Health Response JSON:\n{}", json);
    
    // Verify structure
    assert_eq!(response.status, "degraded");
    assert_eq!(response.version, "0.1.0");
    assert_eq!(response.uptime_seconds, 3600);
    assert_eq!(response.dependencies.len(), 3);
    
    // Verify JSON contains all required fields
    assert!(json.contains("\"status\""));
    assert!(json.contains("\"version\""));
    assert!(json.contains("\"uptime_seconds\""));
    assert!(json.contains("\"dependencies\""));
    assert!(json.contains("\"postgres\""));
    assert!(json.contains("\"redis\""));
    assert!(json.contains("\"horizon\""));
}
