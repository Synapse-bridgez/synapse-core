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
pub mod telemetry;
#[path = "Multi-Tenant Isolation Layer (Architecture)/src/tenant/mod.rs"]
pub mod tenant;
pub mod utils;
pub mod validation;

use crate::db::pool_manager::PoolManager;
use crate::graphql::schema::AppSchema;
use crate::handlers::profiling::ProfilingManager;
use crate::handlers::ws::TransactionStatusUpdate;
pub use crate::readiness::ReadinessState;
use crate::secrets::SecretsStore;
use crate::services::feature_flags::FeatureFlagService;
use crate::services::query_cache::QueryCache;
use crate::stellar::HorizonClient;
use crate::tenant::TenantConfig;
use axum::{
    middleware as axum_middleware,
    routing::{get, patch, post},
    Router,
};
use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio::sync::broadcast;
use uuid::Uuid;

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
    pub query_cache: QueryCache,
    pub profiling_manager: ProfilingManager,
    pub tenant_configs: Arc<tokio::sync::RwLock<HashMap<Uuid, TenantConfig>>>,
    pub secrets_store: Option<SecretsStore>,
    /// Current count of pending transactions, updated every 5s by background task.
    pub pending_queue_depth: Arc<AtomicU64>,
    /// Current adaptive batch size, updated by the processor pool.
    pub current_batch_size: Arc<AtomicU64>,
    /// Prometheus metrics handle
    pub metrics_handle: crate::metrics::MetricsHandle,
}

impl AppState {
    pub async fn get_tenant_config(&self, tenant_id: Uuid) -> Option<TenantConfig> {
        self.tenant_configs.read().await.get(&tenant_id).cloned()
    }

    pub async fn load_tenant_configs(&self) -> anyhow::Result<()> {
        let configs = crate::db::queries::get_all_tenant_configs(&self.db).await?;
        let mut map = self.tenant_configs.write().await;
        map.clear();
        for config in configs {
            map.insert(config.tenant_id, config);
        }
        Ok(())
    }

    pub async fn test_new(database_url: &str) -> Self {
        let pool = sqlx::PgPool::connect(database_url).await.unwrap();
        let (tx, _) = broadcast::channel(100);
        Self {
            db: pool.clone(),
            pool_manager: crate::db::pool_manager::PoolManager::new(database_url, None)
                .await
                .unwrap(),
            horizon_client: HorizonClient::new("https://horizon-testnet.stellar.org".to_string()),
            feature_flags: FeatureFlagService::new(pool),
            redis_url: "redis://localhost:6379".to_string(),
            start_time: std::time::Instant::now(),
            readiness: ReadinessState::new(),
            tx_broadcast: tx,
            query_cache: QueryCache::new("redis://localhost:6379").unwrap(),
            profiling_manager: ProfilingManager::new(),
            tenant_configs: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            secrets_store: None,
            pending_queue_depth: Arc::new(AtomicU64::new(0)),
            current_batch_size: Arc::new(AtomicU64::new(10)),
            metrics_handle: crate::metrics::init_metrics().unwrap(),
        }
    }
}

#[derive(Clone)]
pub struct ApiState {
    pub app_state: AppState,
    pub graphql_schema: AppSchema,
}

impl std::fmt::Debug for ApiState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiState").finish_non_exhaustive()
    }
}

pub fn create_app(app_state: AppState) -> Router {
    let graphql_schema = crate::graphql::schema::build_schema(app_state.clone());
    let api_state = ApiState {
        app_state: app_state.clone(),
        graphql_schema,
    };

    // Callback routes with validation + quota middleware
    let callback_routes = Router::new()
        .route("/callback", post(handlers::webhook::callback))
        .route("/callback/transaction", post(handlers::webhook::callback))
        .layer(axum_middleware::from_fn_with_state(
            app_state.clone(),
            crate::middleware::quota::rate_limit_middleware,
        ))
        .layer(axum_middleware::from_fn(
            crate::middleware::validate::validate_callback,
        ));

    // Webhook route with validation + quota middleware
    let webhook_routes = Router::new()
        .route("/webhook", post(handlers::webhook::handle_webhook))
        .layer(axum_middleware::from_fn_with_state(
            app_state.clone(),
            crate::middleware::quota::rate_limit_middleware,
        ))
        .layer(axum_middleware::from_fn(
            crate::middleware::validate::validate_webhook,
        ));

    // Core API routes (shared between versioned and unversioned)
    let core_routes = Router::new()
        .route("/transactions/:id", get(handlers::webhook::get_transaction))
        .route("/transactions", get(handlers::webhook::list_transactions_api))
        .route("/settlements", get(handlers::settlements::list_settlements))
        .route(
            "/settlements/:id",
            get(handlers::settlements::get_settlement),
        )
        .merge(callback_routes.clone())
        .merge(webhook_routes.clone());

    // V1 routes — stable, with deprecation headers
    let v1_routes = core_routes
        .clone()
        .layer(axum_middleware::from_fn(
            middleware::versioning::v1_version_middleware,
        ));

    // V2 routes — latest, with API-Version: v2 header
    let v2_routes = core_routes
        .clone()
        .layer(axum_middleware::from_fn(
            middleware::versioning::v2_version_middleware,
        ));

    // Admin routes — quota skipped, SecretsStore injected for rotation-aware auth
    let mut admin_router = Router::new()
        .route("/health", get(handlers::health))
        .route("/ready", get(handlers::ready))
        .route("/errors", get(handlers::error_catalog));

    if let Some(store) = &app_state.secrets_store {
        admin_router = admin_router.layer(axum::Extension(store.clone()));
    }

    admin_router
        // Unversioned routes default to V2 behaviour
        .merge(
            core_routes.layer(axum_middleware::from_fn(
                middleware::versioning::v2_version_middleware,
            )),
        )
        // Versioned route groups
        .nest("/api/v1", v1_routes)
        .nest("/api/v2", v2_routes)
        .route(
            "/admin/transactions/bulk-status",
            patch(handlers::admin::bulk_status::bulk_update_status_api),
        )
        .route("/graphql", post(handlers::graphql::graphql_handler))
        .route("/export", get(handlers::export::export_transactions))
        // Stats endpoints
        .route("/stats/status", get(handlers::stats::status_counts))
        .route("/stats/daily", get(handlers::stats::daily_totals))
        .route("/stats/assets", get(handlers::stats::asset_stats))
        .route("/cache/metrics", get(handlers::stats::cache_metrics))
        // Admin: webhook endpoint health scores
        .route("/admin/webhooks/health", get(handlers::admin::list_webhook_health))
        .route("/admin/webhooks/health/:id", get(handlers::admin::get_webhook_health))
        // Admin: per-tenant quota management
        .route("/admin/quotas", get(handlers::admin::quota::list_tenant_quotas))
        .route("/admin/quotas/:tenant_id", get(handlers::admin::quota::get_tenant_quota))
        .route("/admin/quotas/:tenant_id", axum::routing::put(handlers::admin::quota::set_tenant_quota))
        .route("/admin/quotas/:tenant_id/reset", axum::routing::delete(handlers::admin::quota::reset_tenant_quota))
        // Admin: settlement dispute workflow
        .route("/admin/settlements/:id/status", axum::routing::patch(handlers::settlements::update_settlement_status))
        .layer(axum_middleware::from_fn(
            middleware::panic_recovery::panic_recovery_middleware,
        ))
        .with_state(api_state)
        .layer(axum_middleware::from_fn(
            middleware::request_logger::request_logger_middleware,
        ))
}
