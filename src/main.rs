mod cli;
mod config;
mod db;
mod error;
mod handlers;
mod stellar;

use axum::{Router, routing::get};
use clap::Parser;
use cli::{Cli, Commands, TxCommands, DbCommands};
use sqlx::migrate::Migrator;
use std::net::SocketAddr;
use std::path::Path;
use tokio::net::TcpListener;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use stellar::HorizonClient;

#[derive(Clone)]
pub struct AppState {
    db: sqlx::PgPool,
    pub horizon_client: HorizonClient,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = config::Config::from_env()?;

    // Setup logging
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

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

    // Run migrations
    let migrator = Migrator::new(Path::new("./migrations")).await?;
    migrator.run(&pool).await?;
    tracing::info!("Database migrations completed");

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

