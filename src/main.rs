mod adapters;
mod config;
mod db;
mod domain;
mod error;
mod handlers;
mod middleware;
mod ports;
mod stellar;
mod use_cases;

use adapters::PostgresTransactionRepository;
use axum::{
    middleware as axum_middleware,
    routing::{get, post},
    Router,
};
use ports::TransactionRepository;
use sqlx::migrate::Migrator;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use use_cases::ProcessDeposit;

use middleware::idempotency::IdempotencyService;
use stellar::HorizonClient;

#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::PgPool,
    pub horizon_client: HorizonClient,
    pub process_deposit: Arc<ProcessDeposit>,
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

    // Initialize Redis idempotency service
    let idempotency_service = IdempotencyService::new(&config.redis_url)?;
    tracing::info!("Redis idempotency service initialized");

    // Dependency injection: repository and use case
    let transaction_repository: Arc<dyn TransactionRepository> =
        Arc::new(PostgresTransactionRepository::new(pool.clone()));
    let process_deposit = Arc::new(ProcessDeposit::new(transaction_repository));

    // Build router with state
    let app_state = AppState {
        db: pool,
        horizon_client,
        process_deposit,
    };

    // Create webhook routes with idempotency middleware
    let webhook_routes = Router::new()
        .route("/webhook", post(handlers::webhook::handle_webhook))
        .layer(axum_middleware::from_fn_with_state(
            idempotency_service.clone(),
            middleware::idempotency::idempotency_middleware,
        ))
        .with_state(app_state.clone());

    let app = Router::new()
        .route("/health", get(handlers::health))
        .merge(webhook_routes)
        .with_state(app_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.server_port));
    tracing::info!("listening on {}", addr);

    let listener = TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
