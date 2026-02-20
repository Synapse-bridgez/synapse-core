mod config;
mod db;
mod error;
mod handlers;
mod services;
mod stellar;
mod validation;

use axum::{Router, routing::get};
use sqlx::migrate::Migrator; // for Migrator
use std::net::SocketAddr; // for SocketAddr
use std::path::Path; // for Path
use stellar::HorizonClient;
use tokio::net::TcpListener; // for TcpListener
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt}; // for .with() on registry
use stellar::HorizonClient;
use services::SettlementService;

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

    // Build router with state
    let app_state = AppState {
        db: pool,
        horizon_client,
    };
    let app = Router::new()
        .route("/health", get(handlers::health))
        .route("/settlements", get(handlers::settlements::list_settlements))
        .route("/settlements/:id", get(handlers::settlements::get_settlement))
        .with_state(app_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.server_port));
    tracing::info!("listening on {}", addr);

    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
