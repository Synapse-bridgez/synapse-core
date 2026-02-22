mod cli;
mod config;
mod db;
mod error;
mod handlers;
mod health;
mod metrics;
mod middleware;
mod services;
mod stellar;
mod validation;

use axum::{
    Router, 
    routing::get,
    middleware as axum_middleware,
    middleware::Next,
    extract::Request,
    response::Response,
    http::HeaderMap,
    http::StatusCode,
    response::IntoResponse,
    extract::ConnectInfo,
};
use sqlx::migrate::Migrator;
use tower_http::cors::{CorsLayer, AllowOrigin};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tower::{ServiceBuilder, ServiceExt};
use tower_governor::{governor::middleware::NoOpMiddleware, GovernorLayer, key_extractor::KeyExtractor};
use governor::{Quota, RateLimiter};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use clap::Parser;

use crate::cli::{Cli, Commands, TxCommands, DbCommands};
use crate::db::pool_manager::PoolManager;
use crate::services::{SettlementService, feature_flags::FeatureFlagService};
use crate::stellar::HorizonClient;
use crate::middleware::idempotency::IdempotencyService;
use crate::schemas::TransactionStatusUpdate;
use crate::metrics::MetricsState;

#[derive(Clone)]
pub struct AppState {
    db: sqlx::PgPool,
    pub pool_manager: PoolManager,
    pub horizon_client: HorizonClient,
    pub feature_flags: FeatureFlagService,
    pub redis_url: String,
    pub start_time: std::time::Instant,
}

// Custom key extractor for rate limiting
#[derive(Clone)]
struct IpKeyExtractor {
    whitelisted_ips: Arc<tokio::sync::RwLock<Vec<String>>>,
}

impl IpKeyExtractor {
    fn new() -> Self {
        Self {
            whitelisted_ips: Arc::new(tokio::sync::RwLock::new(Vec::new())),
        }
    }

    async fn add_whitelisted_ip(&self, ip: String) {
        let mut whitelist = self.whitelisted_ips.write().await;
        whitelist.push(ip);
    }

    async fn is_whitelisted(&self, ip: &str) -> bool {
        let whitelist = self.whitelisted_ips.read().await;
        whitelist.contains(&ip.to_string())
    }
}

impl KeyExtractor for IpKeyExtractor {
    type Key = String;

    fn extract<B>(&self, req: &Request<B>) -> Result<Self::Key, tower_governor::GovernorError> {
        // Try to get IP from various sources
        let ip = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ci| ci.0.ip().to_string())
            .or_else(|| {
                req.headers()
                    .get("x-forwarded-for")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|s| s.split(',').next())
                    .map(|s| s.trim().to_string())
            })
            .or_else(|| {
                req.headers()
                    .get("x-real-ip")
                    .and_then(|h| h.to_str().ok())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "unknown".to_string());

        Ok(ip)
    }
}

// Rate limiting configuration
#[derive(Clone)]
struct RateLimitConfig {
    default_quota: Quota,
    whitelist_quota: Quota,
    key_extractor: IpKeyExtractor,
}

impl RateLimitConfig {
    fn new(config: &config::Config) -> Self {
        Self {
            default_quota: Quota::per_second(config.default_rate_limit).unwrap(),
            whitelist_quota: Quota::per_second(config.whitelist_rate_limit).unwrap(),
            key_extractor: IpKeyExtractor::new(),
        }
    }

    async fn get_quota_for_ip(&self, ip: &str) -> Quota {
        if self.key_extractor.is_whitelisted(ip).await {
            self.whitelist_quota
        } else {
            self.default_quota
        }
    }

    async fn load_whitelisted_ips(&self, whitelist: &str) {
        for ip in whitelist.split(',').map(|s| s.trim()) {
            if !ip.is_empty() {
                self.key_extractor.add_whitelisted_ip(ip.to_string()).await;
                tracing::info!("Added whitelisted IP: {}", ip);
            }
        }
    }
}

// Rate limiting middleware
async fn rate_limit_middleware<B>(
    req: Request<B>,
    next: Next<B>,
    config: Arc<RateLimitConfig>,
) -> Response {
    // Extract IP
    let ip = config
        .key_extractor
        .extract(&req)
        .unwrap_or_else(|_| "unknown".to_string());

    // Get appropriate quota for this IP
    let quota = config.get_quota_for_ip(&ip).await;
    
    // Create rate limiter for this specific quota
    let rate_limiter = governor::RateLimiter::direct(quota);
    
    // Check rate limit
    match rate_limiter.check() {
        Ok(_) => {
            // Rate limit not exceeded, proceed
            next.run(req).await
        }
        Err(negative) => {
            // Rate limit exceeded, return 429
            let wait_time = negative.wait_time_from(quota.replenish_interval());
            let retry_after = wait_time.as_secs().max(1);
            
            let mut headers = HeaderMap::new();
            headers.insert(
                "x-ratelimit-limit",
                (quota.burst_size().unwrap_or(10) as u64).to_string().parse().unwrap(),
            );
            headers.insert(
                "x-ratelimit-remaining",
                "0".parse().unwrap(),
            );
            headers.insert(
                "retry-after",
                retry_after.to_string().parse().unwrap(),
            );
            
            tracing::warn!("Rate limit exceeded for IP: {}", ip);
            
            (StatusCode::TOO_MANY_REQUESTS, headers, "Too many requests. Please try again later.".to_string()).into_response()
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = config::Config::from_env()?;

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
        Some(Commands::Config) => cli::handle_config_validate(&config),
    }
}

async fn serve(config: config::Config) -> anyhow::Result<()> {
    let pool = db::create_pool(&config).await?;

    // Initialize pool manager for multi-region failover
    let pool_manager = PoolManager::new(
        &config.database_url,
        config.database_replica_url.as_deref(),
    )
    .await?;
    
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
    let settlement_service = SettlementService::new(pool.clone());
    
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
    let metrics_handle = metrics::init_metrics()
        .map_err(|e| anyhow::anyhow!("Failed to initialize metrics: {}", e))?;
    tracing::info!("Metrics initialized successfully");

    // Initialize rate limiting
    let rate_limit_config = Arc::new(RateLimitConfig::new(&config));
    
    // Load whitelisted IPs from config
    if !config.whitelisted_ips.is_empty() {
        rate_limit_config.load_whitelisted_ips(&config.whitelisted_ips).await;
    }
    
    tracing::info!("Rate limiting configured: {} req/sec (default), {} req/sec (whitelisted)", 
                   config.default_rate_limit, config.whitelist_rate_limit);

    // Initialize Redis idempotency service
    let idempotency_service = IdempotencyService::new(&config.redis_url)?;
    tracing::info!("Redis idempotency service initialized");

    // Create broadcast channel for WebSocket notifications
    // Channel capacity of 100 - slow clients will miss old messages (backpressure handling)
    let (tx_broadcast, _) = broadcast::channel::<TransactionStatusUpdate>(100);
    tracing::info!("WebSocket broadcast channel initialized");

    // Initialize feature flags service
    let feature_flags = FeatureFlagService::new(pool.clone());
    tracing::info!("Feature flags service initialized");

    // Build router with state
    let app_state = AppState {
        db: pool.clone(),
        pool_manager,
        horizon_client,
        feature_flags,
        redis_url: config.redis_url.clone(),
        start_time: std::time::Instant::now(),
    };
    
    // Create metrics state
    let metrics_state = MetricsState {
        handle: metrics_handle,
        pool: pool.clone(),
    };
    
    // Create metrics route with authentication middleware
    let metrics_route = Router::new()
        .route("/metrics", get(|
            axum::extract::State(state): axum::extract::State<MetricsState>
        | async move {
            metrics::metrics_handler(
                axum::extract::State(state.handle),
                axum::extract::State(state.pool),
            ).await
        }))
        .layer(axum_middleware::from_fn_with_state(
            config.clone(),
            metrics::metrics_auth_middleware,
        ))
        .with_state(metrics_state);
    
    // Create DLQ routes
    let dlq_routes = handlers::dlq::dlq_routes()
        .with_state(app_state.db.clone());
    
    // Create Admin routes with auth middleware
    let admin_routes = Router::new()
        .nest("/admin/queue", handlers::admin::admin_routes())
        .layer(axum_middleware::from_fn(middleware::auth::admin_auth))
        .with_state(app_state.db.clone());

    let app = Router::new()
        .route("/health", get(handlers::health))
        .route("/settlements", get(handlers::settlements::list_settlements))
        .route("/settlements/:id", get(handlers::settlements::get_settlement))
        .with_state(app_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.server_port));
    tracing::info!("Server listening on {}", addr);

    // Handle graceful shutdown
    let listener = TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
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
