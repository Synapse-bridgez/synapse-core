use clap::Parser;
use opentelemetry::trace::TracerProvider as _;
use sqlx::migrate::Migrator;
use std::{net::SocketAddr, path::Path, sync::atomic::AtomicU64, sync::Arc};
use synapse_core::{
    config, db,
    db::pool_manager::PoolManager,
    handlers,
    handlers::ws::TransactionStatusUpdate,
    metrics,
    middleware::idempotency::IdempotencyService,
    schemas,
    secrets::SecretsStore,
    services::{FeatureFlagService, SettlementService, WebhookDispatcher},
    stellar::HorizonClient,
    telemetry, AppState, ReadinessState,
};
use tokio::sync::broadcast;
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;
mod cli;
use cli::{BackupCommands, Cli, Commands, DbCommands, TxCommands};

/// OpenAPI Schema for the Synapse Core API
#[derive(OpenApi)]
#[openapi(
    paths(
        handlers::health,
        handlers::webhook::handle_webhook,
        handlers::webhook::callback,
        handlers::webhook::get_transaction,
        handlers::webhook::list_transactions,
    ),
    components(
        schemas(
            handlers::HealthStatus,
            handlers::DbPoolStats,
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
    let partition_manager = db::partition::PartitionManager::new(pool.clone(), 24, None);
    partition_manager.start();
    tracing::info!("Partition manager started");

    // Initialize Stellar Horizon client
    let horizon_client = HorizonClient::new(config.stellar_horizon_url.clone());
    tracing::info!(
        "Stellar Horizon client initialized with URL: {}",
        config.stellar_horizon_url
    );

    // Initialize Settlement Service
    let _settlement_service = SettlementService::with_config(
        pool.clone(),
        config.settlement_max_batch_size,
        config.settlement_min_tx_count,
    );

    // Start background settlement worker
    let settlement_pool = pool.clone();
    let settlement_max_batch = config.settlement_max_batch_size;
    let settlement_min_tx = config.settlement_min_tx_count;
    tokio::spawn(async move {
        let service = SettlementService::with_config(
            settlement_pool,
            settlement_max_batch,
            settlement_min_tx,
        );
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

    // Initialize metrics (OTLP exporter + pool stats background task)
    let metrics_handle = metrics::init_metrics()
        .map_err(|e| anyhow::anyhow!("Failed to initialize metrics: {e}"))?;
    tracing::info!("Metrics initialized successfully");
    metrics::spawn_pool_metrics_task(pool.clone(), 30);

    // Initialize rate limiting
    tracing::info!(
        "Rate limiting configured: {} req/min (default), {} req/min (whitelisted)",
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

    // Create broadcast channel for WebSocket notifications.
    // Capacity of 100: slow subscribers will receive a RecvError::Lagged — the WS handler
    // detects this, notifies the client with a "messages_dropped" frame, and offers resync.
    let (tx_broadcast, _) = broadcast::channel::<TransactionStatusUpdate>(100);
    tracing::info!("WebSocket broadcast channel initialized");

    // Initialize feature flags service
    let feature_flags = FeatureFlagService::new(pool.clone());
    tracing::info!("Feature flags service initialized");

    // Initialize secrets store and start rotation task (if Vault is configured).
    let secrets_store = if std::env::var("VAULT_ROLE_ID").is_ok() {
        match synapse_core::secrets::SecretsManager::new().await {
            Ok(manager) => {
                let anchor_secret = manager.get_anchor_secret().await?;
                let admin_key = manager.get_admin_api_key().await?;
                let store = SecretsStore::new(anchor_secret, admin_key);
                manager.start_refresh_task(store.clone());
                tracing::info!("Secrets rotation enabled: refreshing from Vault every 5 minutes");
                Some(store)
            }
            Err(e) => {
                tracing::warn!("Vault unavailable, secrets rotation disabled: {e}");
                None
            }
        }
    } else {
        tracing::info!("Vault not configured, secrets rotation disabled");
        None
    };

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
        secrets_store,
        pending_queue_depth: pending_queue_depth.clone(),
        current_batch_size: current_batch_size.clone(),
        secrets_store,
        metrics_handle,
        ws_connection_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    };

    // Load tenant configs on startup
    if let Err(e) = app_state.load_tenant_configs().await {
        tracing::warn!("Failed to load tenant configs on startup: {}", e);
    } else {
        let count = app_state.tenant_configs.read().await.len();
        tracing::info!(count, "Tenant configs loaded on startup");
    }

    // Background task: reload tenant configs every 60 seconds
    let tenant_reload_state = app_state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            match tenant_reload_state.load_tenant_configs().await {
                Ok(()) => {
                    let count = tenant_reload_state.tenant_configs.read().await.len();
                    tracing::debug!(count, "Tenant configs reloaded (background task)");
                }
                Err(e) => {
                    tracing::error!("Failed to reload tenant configs: {}", e);
                }
            }
        }
    });

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

    // Register and start scheduled jobs
    let scheduler = synapse_core::services::JobScheduler::new();
    let stellar_account = std::env::var("RECONCILIATION_ACCOUNT").ok();

    if let Some(account) = stellar_account {
        let recon_job = synapse_core::services::reconciliation::ReconciliationJob {
            pool: pool.clone(),
            horizon_client: horizon_client.clone(),
            stellar_account: account,
        };
        if let Err(e) = scheduler.register_job(Box::new(recon_job)).await {
            tracing::warn!("Failed to register reconciliation job: {}", e);
        }
    } else {
        tracing::info!(
            "RECONCILIATION_ACCOUNT not set — daily reconciliation job not scheduled"
        );
    }
    if let Err(e) = scheduler.start().await {
        tracing::warn!("Failed to start job scheduler: {}", e);
    }
    tracing::info!("Job scheduler started");

    let app = synapse_core::create_app(app_state);

    // Mount Swagger UI at /api/docs and serve OpenAPI JSON at /api/docs/openapi.json
    let app = app.merge(
        SwaggerUi::new("/api/docs")
            .url("/api/docs/openapi.json", ApiDoc::openapi()),
    );

    // Configure CORS if allowed origins are specified.
    let app = if !config.cors_allowed_origins.is_empty() {
        let origins: Vec<_> = config
            .cors_allowed_origins
            .iter()
            .filter_map(|o| o.parse::<axum::http::HeaderValue>().ok())
            .collect();
        tracing::info!(
            "CORS enabled for origins: {:?}",
            config.cors_allowed_origins
        );
        let cors = CorsLayer::new()
            .allow_origin(AllowOrigin::list(origins))
            .allow_methods(AllowMethods::any())
            .allow_headers(AllowHeaders::any())
            .allow_credentials(true)
            .max_age(std::time::Duration::from_secs(3600));
        app.layer(cors)
    } else {
        tracing::info!("CORS disabled (no allowed origins configured)");
        app
    };

    let addr = SocketAddr::from(([0, 0, 0, 0], config.server_port));
    tracing::info!("listening on {}", addr);

    // Clone readiness state for the shutdown signal handler
    let readiness_for_shutdown = app_state.readiness.clone();

    // Build the shutdown signal: fires on SIGTERM or SIGINT, then drains
    let shutdown_signal = async move {
        let ctrl_c = async {
            tokio::signal::ctrl_c()
                .await
                .expect("failed to install CTRL+C handler");
        };

        #[cfg(unix)]
        let sigterm = async {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to install SIGTERM handler")
                .recv()
                .await;
        };

        #[cfg(not(unix))]
        let sigterm = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => tracing::info!("Received SIGINT, starting graceful shutdown"),
            _ = sigterm => tracing::info!("Received SIGTERM, starting graceful shutdown"),
        }

        // Mark service as not ready so /ready returns 503 immediately
        readiness_for_shutdown.set_not_ready();
        tracing::info!("Readiness set to not_ready; waiting for in-flight requests to drain");

        // Wait for the configured drain timeout (default 30s)
        readiness_for_shutdown.wait_for_drain().await;
    };

    axum::Server::bind(&addr)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async move {
            // Wait for SIGTERM or SIGINT
            #[cfg(unix)]
            {
                use tokio::signal::unix::{signal, SignalKind};
                let mut sigterm =
                    signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
                let mut sigint =
                    signal(SignalKind::interrupt()).expect("failed to register SIGINT handler");
                tokio::select! {
                    _ = sigterm.recv() => tracing::info!("Received SIGTERM"),
                    _ = sigint.recv() => tracing::info!("Received SIGINT"),
                }
            }
            #[cfg(not(unix))]
            {
                tokio::signal::ctrl_c()
                    .await
                    .expect("failed to register Ctrl-C handler");
                tracing::info!("Received Ctrl-C");
            }

            // If not already draining (e.g. /admin/drain was not called), start drain now
            if !readiness.is_draining() {
                readiness.start_drain();
            }
            readiness.wait_for_drain().await;
        })
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
