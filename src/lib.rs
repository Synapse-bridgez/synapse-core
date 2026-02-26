#[path = "Multi-Tenant Isolation Layer (Architecture)/src/tenant/mod.rs"]
pub mod tenant;
pub mod config;
pub mod db;
pub mod error;
pub mod graphql;
pub mod handlers;
pub mod health;
pub mod metrics;
pub mod middleware;
pub mod readiness;
pub mod schemas;
pub mod secrets;
pub mod services;
pub mod startup;
pub mod stellar;
pub mod utils;
pub mod validation;

use crate::db::pool_manager::PoolManager;
use crate::graphql::schema::AppSchema;
use crate::handlers::ws::TransactionStatusUpdate;
pub use crate::readiness::ReadinessState;
use crate::services::feature_flags::FeatureFlagService;
use crate::stellar::HorizonClient;
use axum::{
    routing::{get, post},
    Router,
};
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::PgPool,
    pub pool_manager: PoolManager,
    pub horizon_client: HorizonClient,
    pub feature_flags: FeatureFlagService,
    pub redis_url: String,
    pub start_time: std::time::Instant,
    pub readiness: ReadinessState,
    pub tx_broadcast: broadcast::Sender<TransactionStatusUpdate>,
    // multi-tenant cache
    pub tenant_configs: std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<uuid::Uuid, tenant::TenantConfig>>>,
}

impl AppState {
    /// Create a minimal AppState for testing purposes
    /// only basic fields are initialized -- other services are dummies
    pub async fn test_new(database_url: &str) -> Self {
        let db = sqlx::PgPool::connect(database_url).await.unwrap();
        // pool manager uses same url for primary and no replica
        let pool_manager = crate::db::pool_manager::PoolManager::new(database_url, None)
            .await
            .unwrap();
        let horizon_client = crate::stellar::HorizonClient::new("".to_string());
        let feature_flags = crate::services::feature_flags::FeatureFlagService::new(db.clone());
        let redis_url = String::new();
        let start_time = std::time::Instant::now();
        let readiness = crate::readiness::ReadinessState::new();
        let (tx_broadcast, _) = tokio::sync::broadcast::channel(16);
        let tenant_configs = std::sync::Arc::new(tokio::sync::RwLock::new(
            std::collections::HashMap::new(),
        ));

        AppState {
            db,
            pool_manager,
            horizon_client,
            feature_flags,
            redis_url,
            start_time,
            readiness,
            tx_broadcast,
            tenant_configs,
        }
    }

    /// Load tenant configurations from the database into the in-memory cache
    pub async fn load_tenant_configs(&self) -> Result<(), crate::error::AppError> {
        let configs = crate::db::queries::get_all_tenant_configs(&self.db).await?;
        let mut map = self.tenant_configs.write().await;
        map.clear();
        for config in configs {
            map.insert(config.tenant_id, config);
        }
        Ok(())
    }

    /// Retrieve a configuration from the cache
    pub async fn get_tenant_config(&self, tenant_id: uuid::Uuid) -> Option<tenant::TenantConfig> {
        self.tenant_configs.read().await.get(&tenant_id).cloned()
    }
}

#[derive(Clone)]
pub struct ApiState {
    pub app_state: AppState,
    pub graphql_schema: AppSchema,
}

pub fn create_app(app_state: AppState) -> Router {
    let graphql_schema = crate::graphql::schema::build_schema(app_state.clone());
    let api_state = ApiState {
        app_state,
        graphql_schema,
    };

    Router::new()
        .route("/health", get(handlers::health))
        .route("/ready", get(handlers::ready))
        .route("/errors", get(handlers::error_catalog))
        .route("/settlements", get(handlers::settlements::list_settlements))
        .route(
            "/settlements/:id",
            get(handlers::settlements::get_settlement),
        )
        .route("/callback", post(handlers::webhook::callback))
        .route("/callback/transaction", post(handlers::webhook::callback)) // Backward compatibility
        .route("/transactions/:id", get(handlers::webhook::get_transaction))
        .route("/graphql", post(handlers::graphql::graphql_handler))
        .route("/export", get(handlers::export::export_transactions))
        .with_state(api_state)
}
