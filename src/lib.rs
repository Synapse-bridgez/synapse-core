pub mod auth;
pub mod cache;
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
pub mod security;
pub mod services;
pub mod startup;
pub mod stellar;
pub mod telemetry;
pub mod tenant;
pub mod utils;
pub mod validation;
pub mod ws;

pub use config::assets::AssetCache;

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
    pub allowed_ips: crate::config::AllowedIps,
    pub trusted_proxy_depth: usize,
    pub profiling_manager: ProfilingManager,
    pub tenant_configs: Arc<tokio::sync::RwLock<HashMap<Uuid, TenantConfig>>>,
    pub secrets_store: Option<SecretsStore>,
    /// Current count of pending transactions, updated every 5s by background task.
    pub pending_queue_depth: Arc<AtomicU64>,
    /// Current adaptive batch size, updated by the processor pool.
    pub current_batch_size: Arc<AtomicU64>,
    /// Prometheus metrics handle
    pub metrics_handle: crate::metrics::MetricsHandle,
    /// Admission-controlled pool capping concurrent WebSocket connections.
    pub ws_connection_pool: Arc<crate::ws::connection_pool::ConnectionPool>,
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
        // Uses PgPoolOptions (not the plain PgPool::connect one-liner this
        // used to call) so every connection gets the same
        // set_session_admin_context after_connect hook db::create_pool uses
        // in production — without it, every test that inserts/reads
        // transactions/settlements directly through AppState.db (rather
        // than through the tenant-scoped HTTP routes) would hit RLS with no
        // context at all and fail closed, now that the schema's owning role
        // no longer bypasses RLS.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .after_connect(|conn, _meta| {
                Box::pin(async move { crate::db::set_session_admin_context(conn).await })
            })
            .connect(database_url)
            .await
            .unwrap();
        let (tx, _) = broadcast::channel(100);
        let _asset_cache =
            AssetCache::start(pool.clone(), std::time::Duration::from_secs(300)).await;
        Self {
            db: pool.clone(),
            pool_manager: crate::db::pool_manager::PoolManager::new(database_url, None, 10)
                .await
                .unwrap(),
            horizon_client: HorizonClient::new("https://horizon-testnet.stellar.org".to_string()),
            feature_flags: FeatureFlagService::new(pool),
            redis_url: "redis://localhost:6379".to_string(),
            start_time: std::time::Instant::now(),
            readiness: ReadinessState::new(),
            tx_broadcast: tx,
            query_cache: QueryCache::new("redis://localhost:6379").await.unwrap(),
            allowed_ips: crate::config::AllowedIps::Any,
            trusted_proxy_depth: 1,
            profiling_manager: ProfilingManager::new(),
            tenant_configs: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            secrets_store: None,
            pending_queue_depth: Arc::new(AtomicU64::new(0)),
            current_batch_size: Arc::new(AtomicU64::new(10)),
            metrics_handle: crate::metrics::init_metrics().unwrap(),
            ws_connection_pool: Arc::new(crate::ws::connection_pool::ConnectionPool::new(
                crate::ws::connection_pool::PoolConfig::default(),
            )),
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

/// Lets extractors written against `AppState` (e.g. `TenantContext`) be used
/// directly in handlers whose router state is `ApiState` — axum's substate
/// pattern. Without this, `TenantContext` could only be used in routers
/// keyed on bare `AppState` (like /ws), not the `ApiState`-keyed data routes
/// where it's actually needed.
impl axum::extract::FromRef<ApiState> for AppState {
    fn from_ref(input: &ApiState) -> AppState {
        input.app_state.clone()
    }
}

pub fn create_app(app_state: AppState) -> Router {
    let graphql_schema = crate::graphql::schema::build_schema(app_state.clone());
    let api_state = ApiState {
        app_state: app_state.clone(),
        graphql_schema,
    };

    // Callback routes with IP allowlist + validation + quota middleware.
    // IpFilterLayer is outermost among these three so a request from a
    // non-whitelisted source is rejected before quota/signature validation
    // spend any work on it.
    let callback_routes = Router::new()
        .route("/callback", post(handlers::webhook::callback))
        .route("/callback/transaction", post(handlers::webhook::callback))
        .layer(axum_middleware::from_fn_with_state(
            app_state.clone(),
            crate::middleware::quota::rate_limit_middleware,
        ))
        .layer(axum_middleware::from_fn(
            crate::middleware::validate::validate_callback,
        ))
        .layer(axum_middleware::from_fn_with_state(
            app_state.clone(),
            crate::middleware::webhook_signature::verify_anchor_signature,
        ))
        .layer(crate::middleware::ip_filter::IpFilterLayer::new(
            app_state.allowed_ips.clone(),
            app_state.trusted_proxy_depth,
        ));

    // Webhook route with signature verification + validation + quota middleware
    let webhook_routes = Router::new()
        .route("/webhook", post(handlers::webhook::handle_webhook))
        .layer(axum_middleware::from_fn_with_state(
            app_state.clone(),
            crate::middleware::quota::rate_limit_middleware,
        ))
        .layer(axum_middleware::from_fn(
            crate::middleware::validate::validate_webhook,
        ))
        .layer(axum_middleware::from_fn_with_state(
            app_state.clone(),
            crate::middleware::webhook_signature::verify_anchor_signature,
        ));

    // Tenant-scoped data routes. These previously had zero auth of any kind —
    // core_routes was built on a bare `Router::new()` with no `.layer()` of
    // its own, so the version-header middleware applied at each mount point
    // (below) was the *only* middleware ever wrapping them.
    //
    // Auth here is enforced per-handler via the `TenantContext` extractor
    // (src/tenant/mod.rs) rather than a blanket `api_key_auth` middleware
    // layer: these handlers need the *resolved tenant_id* to scope each
    // query by, not just a yes/no auth decision, and a boolean middleware
    // can't hand a resolved value to the handler it wraps. Applying both
    // would also be wrong here for a second reason — see the note on
    // `core_routes` below.
    let data_routes = Router::new()
        .route("/transactions/:id", get(handlers::webhook::get_transaction))
        .route(
            "/transactions",
            get(handlers::webhook::list_transactions_api),
        )
        .route(
            "/transactions/search",
            get(handlers::search::search_transactions_wrapper),
        )
        .route("/settlements", get(handlers::settlements::list_settlements))
        .route(
            "/settlements/:id",
            get(handlers::settlements::get_settlement),
        );

    // core_routes intentionally does NOT layer api_key_auth across the board:
    // callback_routes/webhook_routes authenticate inbound anchor calls via
    // HMAC signature validation (middleware::webhook_signature::verify_anchor_signature),
    // not a tenant API key — a blanket api_key_auth layer here would reject
    // every legitimate webhook delivery. validate_callback/validate_webhook
    // (also layered on these routes) check payload *shape* only — despite
    // the similar names, they perform no cryptographic verification; before
    // webhook_signature::verify_anchor_signature was added, this comment
    // incorrectly described validate_callback/validate_webhook themselves as
    // the HMAC check, and no HMAC check actually ran anywhere on this path.
    // Only data_routes needs tenant-key auth, and it gets it from the
    // TenantContext extractor above.
    let core_routes = data_routes
        .merge(callback_routes.clone())
        .merge(webhook_routes.clone());

    // V1 routes — stable, with deprecation headers
    let v1_routes = core_routes.clone().layer(axum_middleware::from_fn(
        middleware::versioning::v1_version_middleware,
    ));

    // V2 routes — latest, with API-Version: v2 header
    let v2_routes = core_routes.clone().layer(axum_middleware::from_fn(
        middleware::versioning::v2_version_middleware,
    ));

    // Health/liveness routes — intentionally public (infra probes have no
    // credentials to send) and never in scope for admin_auth.
    let public_health_routes = Router::new()
        .route("/live", get(handlers::live))
        .route("/ready", get(handlers::ready))
        .route("/health", get(handlers::health))
        .route("/errors", get(handlers::error_catalog));

    // Admin-only routes. `admin_auth` exists in src/middleware/auth.rs but,
    // before this fix, had zero callers anywhere in the router — every route
    // below (quota overrides, webhook health, distributed locks, settlement
    // status changes, reconciliation reports, bulk status updates, GraphQL,
    // export, stats) was reachable with no credentials at all. This is worse
    // than what the tracked issue described (it assumed admin_auth already
    // covered these) — see "Also fixes" in the PR description.
    let mut admin_only_routes = Router::new()
        .route(
            "/admin/transactions/bulk-status",
            patch(handlers::admin::bulk_status::bulk_update_status_api),
        )
        .route(
            "/admin/transactions/bulk-status/jobs/:id",
            get(handlers::admin::bulk_status::get_job_status),
        )
        .route("/graphql", post(handlers::graphql::graphql_handler))
        .route("/export", get(handlers::export::export_transactions))
        // Stats endpoints
        .route("/stats/status", get(handlers::stats::status_counts))
        .route("/stats/daily", get(handlers::stats::daily_totals))
        .route("/stats/assets", get(handlers::stats::asset_stats))
        .route("/cache/metrics", get(handlers::stats::cache_metrics))
        // Admin: webhook endpoint health scores
        .route(
            "/admin/webhooks/health",
            get(handlers::admin::list_webhook_health),
        )
        .route(
            "/admin/webhooks/health/:id",
            get(handlers::admin::get_webhook_health),
        )
        // Admin: per-tenant quota management
        .route(
            "/admin/quotas",
            get(handlers::admin::quota::list_tenant_quotas),
        )
        .route(
            "/admin/quotas/:tenant_id",
            get(handlers::admin::quota::get_tenant_quota),
        )
        .route(
            "/admin/quotas/:tenant_id",
            axum::routing::put(handlers::admin::quota::set_tenant_quota),
        )
        .route(
            "/admin/quotas/:tenant_id/reset",
            axum::routing::delete(handlers::admin::quota::reset_tenant_quota),
        )
        // Admin: active distributed locks
        .route(
            "/admin/locks",
            get(handlers::admin::locks::list_active_locks),
        )
        // Admin: audit log search — fully implemented and unit-tested since
        // before this fix, but never mounted anywhere; see
        // docs/audit-compliance-admin-endpoints.md.
        .route(
            "/admin/audit/search",
            get(handlers::admin::audit::search_audit_logs_handler),
        )
        // Admin: compliance report generation/listing — same gap as audit
        // search above.
        .route(
            "/admin/compliance/reports",
            post(handlers::admin::compliance::generate_report)
                .get(handlers::admin::compliance::list_reports),
        )
        // Admin: settlement dispute workflow
        .route(
            "/admin/settlements/:id/status",
            axum::routing::patch(handlers::settlements::update_settlement_status),
        )
        // Admin: reconciliation reports
        .nest(
            "/admin/reconciliation",
            handlers::admin::reconciliation::reconciliation_routes(),
        )
        .layer(axum_middleware::from_fn(middleware::auth::admin_auth));

    // SecretsStore must be the outermost layer here (axum applies the *last*
    // `.layer()` call as outermost) so admin_auth's rotation-aware check can
    // read the extension `req.extensions().get::<SecretsStore>()` expects —
    // if this were applied before the admin_auth layer instead, admin_auth
    // would run first and never see it.
    if let Some(store) = &app_state.secrets_store {
        admin_only_routes = admin_only_routes.layer(axum::Extension(store.clone()));
    }

    public_health_routes
        // Unversioned routes default to V2 behaviour
        .merge(core_routes.layer(axum_middleware::from_fn(
            middleware::versioning::v2_version_middleware,
        )))
        // Versioned route groups
        .nest("/api/v1", v1_routes)
        .nest("/api/v2", v2_routes)
        .merge(admin_only_routes)
        .layer(axum_middleware::from_fn(
            middleware::panic_recovery::panic_recovery_middleware,
        ))
        .with_state(api_state)
        // /reconnect/status and /reconnect were removed here — see "Also
        // fixes" in the PR description (Part D). They were unauthenticated,
        // grew an in-memory session map without bound (the one function that
        // evicted stale entries was never called from anywhere), and — most
        // importantly — were never actually consulted by ws_handler at all.
        // A client calling /reconnect/status got a session_id and backoff
        // recommendation with zero bearing on its real WebSocket connection.
        // Patching auth and a cleanup schedule onto that would have made it
        // secure but still misleading; removing it is the smaller, more
        // honest change.
        .merge(
            Router::new()
                .route("/ws", get(handlers::ws::ws_handler))
                .with_state(app_state),
        )
        // NOTE: axum applies the *last* `.layer()` call as the *outermost* wrapper,
        // so it runs first on the request path and last on the response path.
        // `request_logger` must stay outermost relative to `error_enrichment`:
        // it sets the `RequestId` extension before `next.run()`, which
        // `error_enrichment` reads before its own `next.run()`. Reversing this
        // order makes every enriched error body report `request_id: "unknown"`.
        .layer(axum_middleware::from_fn(
            middleware::error_enrichment::error_enrichment_middleware,
        ))
        .layer(axum_middleware::from_fn(
            middleware::request_logger::request_logger_middleware,
        ))
}
