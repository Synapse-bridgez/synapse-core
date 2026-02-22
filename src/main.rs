mod config;
mod db;
mod error;
mod handlers;
mod middleware;
mod stellar;
mod services;

use axum::{Router, extract::State, routing::{get, post}, middleware as axum_middleware};
use http::header::HeaderValue;
use sqlx::migrate::Migrator; // for Migrator
use tower_http::cors::{CorsLayer, AllowOrigin};
use std::net::SocketAddr; // for SocketAddr
use std::path::Path; // for Path
use tokio::net::TcpListener; // for TcpListener
use tracing_subscriber::prelude::*;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt}; // for .with() on registry
use stellar::HorizonClient;
use middleware::idempotency::IdempotencyService;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

/// OpenAPI Schema for the Synapse Core API
#[derive(OpenApi)]
#[openapi(
    paths(
        handlers::health,
        handlers::settlements::list_settlements,
        handlers::settlements::get_settlement,
        handlers::webhook::handle_webhook,
        handlers::webhook::get_transaction,
        handlers::admin::get_queue_status,
        handlers::admin::get_failed_transactions,
        handlers::admin::retry_transaction,
    ),
    components(
        schemas(
            handlers::HealthStatus,
            handlers::settlements::Pagination,
            handlers::settlements::SettlementListResponse,
            handlers::webhook::WebhookPayload,
            handlers::webhook::WebhookResponse,
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

    // Initialize Redis idempotency service
    let idempotency_service = IdempotencyService::new(&config.redis_url)?;
    tracing::info!("Redis idempotency service initialized");

    // Build CORS layer from configurable origins
    let cors_layer = match &config.cors_allowed_origins {
        Some(origins) => {
            let origins: Vec<HeaderValue> = origins
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            CorsLayer::new().allow_origin(AllowOrigin::list(origins))
        }
        None => CorsLayer::permissive(), // Allow any origin when not configured (dev default)
    };

    // Build router with state
    let app_state = AppState {
        db: pool,
        horizon_client,
    };
    
    // Create webhook routes with idempotency middleware
    let webhook_routes = Router::new()
        .route("/webhook", post(handlers::webhook::handle_webhook))
        .layer(axum_middleware::from_fn_with_state(
            idempotency_service.clone(),
            middleware::idempotency::idempotency_middleware,
        ))
        .with_state(app_state.clone());
    
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
        .merge(webhook_routes)
        .merge(dlq_routes)
        .merge(admin_routes)
        .layer(cors_layer)
        .with_state(app_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.server_port));
    tracing::info!("listening on {}", addr);
    tracing::info!("Swagger UI available at http://localhost:{}/swagger-ui/", config.server_port);

    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;

    Ok(())
}

