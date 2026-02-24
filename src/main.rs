use axum::{
    middleware as axum_middleware,
    routing::{get, post},
    Router,
};
use clap::Parser;
use sqlx::migrate::Migrator;
use std::{net::SocketAddr, path::Path};
use synapse_core::{
    config, db,
    db::pool_manager::PoolManager,
    graphql::schema::build_schema,
    handlers,
    handlers::ws::TransactionStatusUpdate,
    metrics, middleware,
    middleware::idempotency::IdempotencyService,
    schemas,
    services::{FeatureFlagService, SettlementService},
    stellar::HorizonClient,
    ApiState, AppState, ReadinessState,
};
use tokio::sync::broadcast;
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

    // Setup logging
    let env_filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    match config.log_format {
        config::LogFormat::Json => {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(tracing_subscriber::fmt::layer().json())
                .init();
        }
        config::LogFormat::Text => {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(tracing_subscriber::fmt::layer())
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
    let _idempotency_service = IdempotencyService::new(&config.redis_url)?;
    tracing::info!("Redis idempotency service initialized");

    // Create broadcast channel for WebSocket notifications
    // Channel capacity of 100 - slow clients will miss old messages (backpressure handling)
    let (tx_broadcast, _) = broadcast::channel::<TransactionStatusUpdate>(100);
    tracing::info!("WebSocket broadcast channel initialized");

    // Initialize feature flags service
    let feature_flags = FeatureFlagService::new(pool.clone());
    tracing::info!("Feature flags service initialized");

    let monitor_pool = pool.clone();
    let app_state = AppState {
        db: pool.clone(),
        pool_manager,
        horizon_client,
        feature_flags,
        redis_url: config.redis_url.clone(),
        start_time: std::time::Instant::now(),
        readiness: ReadinessState::new(),
        tx_broadcast,
    };

    let graphql_schema = build_schema(app_state.clone());
    let api_state = ApiState {
        app_state,
        graphql_schema,
    };

    tokio::spawn(async move {
        pool_monitor_task(monitor_pool).await;
    });

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
        .layer(axum_middleware::from_fn(middleware::auth::admin_auth))
        .with_state(api_state.app_state.db.clone());

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
        .with_state(api_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.server_port));
    tracing::info!("listening on {}", addr);

    axum::Server::bind(&addr)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .await?;

    Ok(())
}

/// Background task to monitor database connection pool usage
async fn pool_monitor_task(pool: sqlx::PgPool) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));

    loop {
        interval.tick().await;

        let active = pool.size();
        let idle = pool.num_idle();
        let max = pool.options().get_max_connections();
        let usage_percent = (active as f32 / max as f32) * 100.0;

        // Log warning if pool usage exceeds 80%
        if usage_percent >= 80.0 {
            tracing::warn!(
                "Database connection pool usage high: {:.1}% ({}/{} connections active, {} idle)",
                usage_percent,
                active,
                max,
                idle
            );
        } else {
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
