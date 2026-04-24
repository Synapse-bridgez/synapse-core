use axum::{
    middleware as axum_middleware,
    routing::{get, post},
    Router,
};
use clap::Parser;
use opentelemetry::trace::TracerProvider as _;
use sqlx::migrate::Migrator;
use std::{net::SocketAddr, path::Path, sync::atomic::AtomicU64, sync::Arc};
use synapse_core::{
    config, db,
    db::pool_manager::PoolManager,
    graphql::schema::build_schema,
    handlers,
    handlers::ws::TransactionStatusUpdate,
    metrics, middleware,
    middleware::idempotency::IdempotencyService,
    schemas,
    services::{FeatureFlagService, LeaderElection, SettlementService, WebhookDispatcher},
    stellar::HorizonClient,
    telemetry, ApiState, AppState, ReadinessState,
};
use tokio::sync::broadcast;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use utoipa::OpenApi;
mod cli;
use cli::{BackupCommands, Cli, Commands, DbCommands, TxCommands};

/// OpenAPI Schema for the Synapse Core API
#[derive(OpenApi)]
#[openapi(
    paths(
        handlers::health,
        handlers::settlements::list_settlements,
        handlers::settlements::get_settlement,
        handlers::webhook::handle_webhook,
        handlers::webhook::callback,
        handlers::webhook::get_transaction,
    ),
    components(
        schemas(
            handlers::HealthStatus,
            handlers::DbPoolStats,
            handlers::settlements::Pagination,
            handlers::settlements::SettlementListResponse,
            handlers::webhook::WebhookPayload,
            handlers::webhook::WebhookResponse,
            handlers::webhook::CallbackPayload,
            schemas::TransactionSchema,
            schemas::SettlementSchema,
        )
    ),
    info(
        title = "Synapse Core API",
        version = "0.1.0",
        description = "Settlement and transaction management API for the Stellar network",
        contact(name = "Synapse Team")
    ),
    tags(
        (name = "Health", description = "Health check endpoints"),
        (name = "Settlements", description = "Settlement management endpoints"),
        (name = "Transactions", description = "Transaction management endpoints"),
        (name = "Webhooks", description = "Webhook callback endpoints"),
    )
)]
pub struct ApiDoc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = config::Config::load().await?;

    // Setup logging + OpenTelemetry tracing layer
    let env_filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());

    // Init OTel tracer early so the tracing layer can reference it.
    let tracer_provider = telemetry::init_tracer("synapse-core", config.otlp_endpoint.as_deref())
        .expect("failed to initialise OpenTelemetry tracer");

    match config.log_format {
        config::LogFormat::Json => {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(tracing_subscriber::fmt::layer().json())
                .with(OpenTelemetryLayer::new(
                    tracer_provider.tracer("synapse-core"),
                ))
                .init();
        }
        config::LogFormat::Text => {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(tracing_subscriber::fmt::layer())
                .with(OpenTelemetryLayer::new(
                    tracer_provider.tracer("synapse-core"),
                ))
                .init();
        }
    }

    match cli.command {
        Some(Commands::Serve) | None => serve(config).await,
        Some(Commands::Tx(tx_cmd)) => match tx_cmd {
            TxCommands::ForceComplete { tx_id } => {
                let pool = db::create_pool(&config).await?;
                cli::handle_tx_force_complete(&pool, tx_id).await
            }
            TxCommands::Reconcile {
                account,
                start,
                end,
                format,
            } => cli::handle_tx_reconcile(&config, &account, &start, &end, &format).await,
        },
        Some(Commands::Db(db_cmd)) => match db_cmd {
            DbCommands::Migrate => cli::handle_db_migrate(&config).await,
        },
        Some(Commands::Backup(backup_cmd)) => match backup_cmd {
            BackupCommands::Run { backup_type } => {
                cli::handle_backup_run(&config, &backup_type).await
            }
            BackupCommands::List => cli::handle_backup_list(&config).await,
            BackupCommands::Restore { filename } => {
                cli::handle_backup_restore(&config, &filename).await
            }
            BackupCommands::Cleanup => cli::handle_backup_cleanup(&config).await,
        },
        Some(Commands::Config) => cli::handle_config_validate(&config),
    }
}

async fn serve(config: config::Config) -> anyhow::Result<()> {
    let pool = db::create_pool(&config).await?;

    // Initialize pool manager for multi-region failover
    let pool_manager =
        PoolManager::new(&config.database_url, config.database_replica_url.as_deref()).await?;

    if pool_manager.replica().is_some() {
        tracing::info!("Database replica configured - read queries will be routed to replica");
    } else {
        tracing::info!("No replica configured - all queries will use primary database");
    }

    // Run migrations
    let migrator = Migrator::new(Path::new("./migrations")).await?;
    migrator.run(&pool).await?;
    tracing::info!("Database migrations completed");

    // Initialize partition manager (runs every 24 hours)
    let partition_manager = db::partition::PartitionManager::new(pool.clone(), 24);
    partition_manager.start();
    tracing::info!("Partition manager started");

    // Initialize Stellar Horizon client
    let horizon_client = HorizonClient::new(config.stellar_horizon_url.clone());
    tracing::info!(
        "Stellar Horizon client initialized with URL: {}",
        config.stellar_horizon_url
    );

    // Initialize Settlement Service
    let _settlement_service = SettlementService::new(pool.clone());

    // Start background settlement worker
    let settlement_pool = pool.clone();
    tokio::spawn(async move {
        let service = SettlementService::new(settlement_pool);
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(3600)); // Default to hourly
        loop {
            interval.tick().await;
            tracing::info!("Running scheduled settlement job...");
            match service.run_settlements().await {
                Ok(results) => {
                    if !results.is_empty() {
                        tracing::info!("Successfully generated {} settlements", results.len());
                    }
                }
                Err(e) => tracing::error!("Scheduled settlement job failed: {:?}", e),
            }
        }
    });

    // Start background webhook delivery worker (runs every 30 seconds)
    let webhook_pool = pool.clone();
    let redis_url = config.redis_url.clone();
    tokio::spawn(async move {
        let dispatcher = WebhookDispatcher::new(webhook_pool, &redis_url)
            .expect("failed to create webhook dispatcher");
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            if let Err(e) = dispatcher.process_pending().await {
                tracing::error!("Webhook dispatcher error: {e}");
            }
        }
    });
    tracing::info!("Webhook dispatcher background worker started");

    // Initialize metrics
    let _metrics_handle = metrics::init_metrics()
        .map_err(|e| anyhow::anyhow!("Failed to initialize metrics: {}", e))?;
    tracing::info!("Metrics initialized successfully");

    // Initialize rate limiting
    // let rate_limit_config = Arc::new(RateLimitConfig::new(&config));

    // Load whitelisted IPs from config
    // if !config.whitelisted_ips.is_empty() {
    //     rate_limit_config.load_whitelisted_ips(&config.whitelisted_ips).await;
    // }

    tracing::info!(
        "Rate limiting configured: {} req/sec (default), {} req/sec (whitelisted)",
        config.default_rate_limit,
        config.whitelist_rate_limit
    );

    // Initialize Redis idempotency service
    let idempotency_cache_hits = Arc::new(AtomicU64::new(0));
    let idempotency_cache_misses = Arc::new(AtomicU64::new(0));
    let idempotency_lock_acquired = Arc::new(AtomicU64::new(0));
    let idempotency_lock_contention = Arc::new(AtomicU64::new(0));
    let idempotency_errors = Arc::new(AtomicU64::new(0));
    let idempotency_fallback_count = Arc::new(AtomicU64::new(0));
    let _idempotency_service = IdempotencyService::new(
        &config.redis_url,
        pool.clone(),
        Arc::clone(&idempotency_cache_hits),
        Arc::clone(&idempotency_cache_misses),
        Arc::clone(&idempotency_lock_acquired),
        Arc::clone(&idempotency_lock_contention),
        Arc::clone(&idempotency_errors),
        Arc::clone(&idempotency_fallback_count),
    )?;
    tracing::info!("Redis idempotency service initialized");

    // Initialize query cache
    let query_cache = synapse_core::services::QueryCache::new(&config.redis_url)?;
    tracing::info!("Query cache initialized");

    // Warm cache on startup
    let cache_config = synapse_core::services::CacheConfig::default();
    if let Err(e) = query_cache.warm_cache(&pool, &cache_config).await {
        tracing::warn!("Failed to warm cache on startup: {:?}", e);
    }

    // Create broadcast channel for WebSocket notifications
    // Channel capacity of 100 - slow clients will miss old messages (backpressure handling)
    let (tx_broadcast, _) = broadcast::channel::<TransactionStatusUpdate>(100);
    tracing::info!("WebSocket broadcast channel initialized");

    // Initialize feature flags service
    let feature_flags = FeatureFlagService::new(pool.clone());
    tracing::info!("Feature flags service initialized");

    let monitor_pool = pool.clone();
    let pending_queue_depth = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let current_batch_size = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(
        config.processor_min_batch as u64,
    ));
    let app_state = AppState {
        db: pool.clone(),
        pool_manager,
        horizon_client: horizon_client.clone(),
        feature_flags,
        redis_url: config.redis_url.clone(),
        start_time: std::time::Instant::now(),
        readiness: ReadinessState::new(),
        tx_broadcast,
        query_cache,
        profiling_manager: crate::handlers::profiling::ProfilingManager::new(),
        tenant_configs: std::sync::Arc::new(tokio::sync::RwLock::new(
            std::collections::HashMap::new(),
        )),
        pending_queue_depth: pending_queue_depth.clone(),
        current_batch_size: current_batch_size.clone(),
    };

    let graphql_schema = build_schema(app_state.clone());
    let api_state = ApiState {
        app_state,
        graphql_schema,
    };

    tokio::spawn(async move {
        pool_monitor_task(monitor_pool).await;
    });

    // Back-pressure: refresh pending queue depth every 5s
    let depth_pool = pool.clone();
    let depth_counter = pending_queue_depth.clone();
    tokio::spawn(async move {
        synapse_core::services::processor::queue_depth_task(depth_pool, depth_counter).await;
    });

    // Concurrent processor pool
    let processor_pool = synapse_core::services::processor::ProcessorPool::new(
        pool.clone(),
        horizon_client,
        config.processor_workers,
        config.processor_poll_interval_ms,
        config.processor_min_batch,
        config.processor_max_batch,
        config.processor_scaling_factor,
        current_batch_size,
        pending_queue_depth,
    );
    let _processor_shutdown = processor_pool.start();

    let _api_routes: Router = Router::new()
        .route("/health", get(handlers::health))
        .route("/settlements", get(handlers::settlements::list_settlements))
        .route(
            "/settlements/:id",
            get(handlers::settlements::get_settlement),
        )
        .route("/callback", post(handlers::webhook::callback))
        .route("/transactions/:id", get(handlers::webhook::get_transaction))
        .route("/graphql", post(handlers::graphql::graphql_handler))
        .with_state(api_state.clone());

    let _webhook_routes: Router = Router::new()
        .route("/webhook", post(handlers::webhook::handle_webhook))
        .layer(axum_middleware::from_fn_with_state(
            config.clone(),
            metrics::metrics_auth_middleware::<axum::body::Body>,
        ))
        .with_state(api_state.clone());

    let _dlq_routes: Router =
        handlers::dlq::dlq_routes().with_state(api_state.app_state.db.clone());

    let _admin_routes: Router = Router::new()
        .nest("/admin/queue", handlers::admin::admin_routes())
        .nest("/admin", handlers::admin::webhook_replay_routes())
        .layer(axum_middleware::from_fn(middleware::auth::admin_auth))
        .with_state(api_state.app_state.db.clone());
    // Admin routes disabled - requires AdminState setup
    // let _admin_routes: Router = Router::new()
    //     .nest("/admin/queue", handlers::admin::admin_routes())
    //     .layer(axum_middleware::from_fn(middleware::auth::admin_auth))
    //     .with_state(api_state.app_state.db.clone());

    let _admin_profiling_routes: Router = Router::new()
        .route("/start", post(handlers::profiling::start_profiling))
        .route("/status", get(handlers::profiling::get_profiling_status))
        .route("/stop", post(handlers::profiling::stop_profiling))
        .route(
            "/flamegraph/:session_id",
            get(handlers::profiling::get_flamegraph),
        )
        .layer(axum_middleware::from_fn(middleware::auth::admin_auth))
        .with_state(api_state.app_state.clone());

    let _search_routes: Router = Router::new()
        .route(
            "/transactions/search",
            get(handlers::search::search_transactions),
        )
        .with_state(api_state.app_state.pool_manager.clone());

    let app = Router::new()
        // Unversioned routes - default to latest (V2) or specific base routes
        .route("/health", get(handlers::health))
        .route("/settlements", get(handlers::settlements::list_settlements))
        .route(
            "/settlements/:id",
            get(handlers::settlements::get_settlement),
        )
        .route(
            "/admin/instances",
            get(handlers::admin::list_active_instances),
        )
        .with_state(api_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.server_port));
    tracing::info!("listening on {}", addr);

    axum::Server::bind(&addr)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .await?;

    // Flush and shut down the OTel exporter on clean exit.
    opentelemetry::global::shutdown_tracer_provider();

    Ok(())
}

/// Background task to monitor database connection pool usage
async fn pool_monitor_task(pool: sqlx::PgPool) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
    let mut consecutive_high: u32 = 0;

    loop {
        interval.tick().await;

        let active = pool.size();
        let idle = pool.num_idle();
        let max = pool.options().get_max_connections();
        let usage_percent = (active as f32 / max as f32) * 100.0;

        if usage_percent >= 80.0 {
            consecutive_high += 1;
            if consecutive_high >= 3 {
                tracing::error!(
                    "CRITICAL: Database connection pool usage has been ≥80% for {} consecutive checks: \
                     {:.1}% ({}/{} active, {} idle)",
                    consecutive_high,
                    usage_percent,
                    active,
                    max,
                    idle
                );
            } else {
                tracing::warn!(
                    "Database connection pool usage high: {:.1}% ({}/{} connections active, {} idle)",
                    usage_percent,
                    active,
                    max,
                    idle
                );
            }
        } else {
            consecutive_high = 0;
            tracing::debug!(
                "Database connection pool status: {:.1}% ({}/{} connections active, {} idle)",
                usage_percent,
                active,
                max,
                idle
            );
        }
    }
}
