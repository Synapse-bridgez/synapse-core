mod config;
mod db;
mod error;
mod handlers;
mod stellar;

use axum::{Router, routing::get};
use sqlx::migrate::Migrator; // for Migrator
use std::net::SocketAddr; // for SocketAddr
use std::path::Path; // for Path
use tokio::net::TcpListener; // for TcpListener
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt}; // for .with() on registry
use stellar::HorizonClient;

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

    // Initialize partition management background job
    let partition_pool = pool.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(86400)); // Run daily
        loop {
            interval.tick().await;
            tracing::info!("Running partition maintenance job");
            match db::cron::PartitionManager::run_maintenance(&partition_pool).await {
                Ok(_) => tracing::info!("Partition maintenance job completed successfully"),
                Err(e) => tracing::error!("Partition maintenance job failed: {}", e),
            }
        }
    });

    // Initialize Stellar Horizon client
    let horizon_client = HorizonClient::new(config.stellar_horizon_url.clone());
    tracing::info!("Stellar Horizon client initialized with URL: {}", config.stellar_horizon_url);

    // Build router with state
    let app_state = AppState { 
        db: pool,
        horizon_client,
    };
    let app = Router::new()
        .route("/health", get(handlers::health))
        .with_state(app_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.server_port));
    tracing::info!("listening on {}", addr);

    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

