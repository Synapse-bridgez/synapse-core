mod config;
mod db;
mod error;
mod handlers;
mod health;
// mod middleware; // Temporarily disabled
mod stellar;
mod services;
mod schemas;
mod utils;

use axum::{Router, routing::{get, post}};
use sqlx::migrate::Migrator;
use std::net::SocketAddr;
use std::path::Path;
use tokio::net::TcpListener;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use utoipa::OpenApi;

/// OpenAPI Schema for the Synapse Core API
#[derive(OpenApi)]
#[openapi(
    paths(
        handlers::health,
    ),
    components(
        schemas(
            handlers::HealthStatus,
            health::HealthResponse,
            health::DependencyStatus,
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
    )
)]
pub struct ApiDoc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_info = config::Config::from_env()?;
    let config = config_info.config;

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

    // Build app state using the types from main
    let app_state = synapse_core::AppState {
        db: pool.clone(),
        horizon_client: synapse_core::stellar::HorizonClient::new(config.stellar_horizon_url.clone()),
        health_checker: std::sync::Arc::new(
            synapse_core::health::HealthChecker::new()
                .add_checker(Box::new(synapse_core::health::PostgresChecker::new(pool.clone())))
                .add_checker(Box::new(synapse_core::health::RedisChecker::new(config.redis_url.clone())))
                .add_checker(Box::new(synapse_core::health::HorizonChecker::new(
                    synapse_core::stellar::HorizonClient::new(config.stellar_horizon_url.clone())
                )))
        ),
    };
    
    // Use the lib's create_app function
    let app = synapse_core::create_app(app_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.server_port));
    tracing::info!("listening on {}", addr);

    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;

    Ok(())
}

