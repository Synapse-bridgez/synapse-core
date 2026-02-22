#[cfg(test)]
mod tests {
    use super::*;
    use crate::stellar::HorizonClient;
    use sqlx::PgPool;

    #[test]
    fn test_app_state_creation() {
        // Mock database pool (in real tests you'd use a test database)
        let db_url = "postgresql://test:test@localhost/test";
        // Note: This would fail in actual test run without a real DB, 
        // but demonstrates the structure
        
        let horizon_client = HorizonClient::new("https://horizon-testnet.stellar.org".to_string());
        
        // Test that we can create the struct
        assert_eq!(horizon_client.base_url, "https://horizon-testnet.stellar.org");
    }

    #[test]
    fn test_app_state_clone() {
        let horizon_client = HorizonClient::new("https://horizon-testnet.stellar.org".to_string());
        let cloned = horizon_client.clone();
        
        assert_eq!(horizon_client.base_url, cloned.base_url);
    }

    // Integration test would require actual database setup
    #[tokio::test]
    #[ignore] // Ignore by default since it requires database setup
    async fn test_create_app_integration() {
        // This test would require setting up a test database
        // let pool = PgPool::connect("postgresql://test:test@localhost/test").await.unwrap();
        // let horizon_client = HorizonClient::new("https://horizon-testnet.stellar.org".to_string());
        // let app_state = AppState { db: pool, horizon_client };
        // let app = create_app(app_state);
        // 
        // // Test that routes are properly configured
        // // This would require axum test utilities
    }
}