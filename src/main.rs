mod config;
mod db;
mod error;
mod handlers;
mod metrics;
mod stellar;

use axum::{Router, routing::get};
use sqlx::migrate::Migrator;
use std::net::SocketAddr;
use std::path::Path;
use tokio::net::TcpListener;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use stellar::HorizonClient;
use std::time::Duration;

#[derive(Clone)] // <-- Add Clone
pub struct AppState {
    db: sqlx::PgPool,
    pub horizon_client: HorizonClient,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = config::Config::from_env()?;

    // Setup logging
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Database pool
    let pool = db::create_pool(&config).await?;

    // Run migrations
    let migrator = Migrator::new(Path::new("./migrations")).await?;
    migrator.run(&pool).await?;
    tracing::info!("Database migrations completed");

    // Initialize Stellar Horizon client
    let horizon_client = HorizonClient::new(config.stellar_horizon_url.clone());
    tracing::info!("Stellar Horizon client initialized with URL: {}", config.stellar_horizon_url);

    // Build router with state
    let app_state = AppState { 
        db: pool.clone(),
        horizon_client,
    };
    
    // Spawn pool monitoring task
    let pool_clone = pool.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            let metrics = metrics::PoolMetrics::from_pool(&pool_clone);
            metrics::emit_metrics(&metrics);
        }
    });
    
    let app = Router::new()
        .route("/health", get(handlers::health))
        .with_state(app_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.server_port));
    tracing::info!("listening on {}", addr);

    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

