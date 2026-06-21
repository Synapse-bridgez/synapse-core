use axum::{
    body::Body,
    http::Request,
    middleware::Next,
    response::Response,
};
use uuid::Uuid;

use crate::tenant::TenantContext;

/// Middleware that extracts tenant context from the request and stores it in extensions.
/// The context is then available to handlers and can be used to establish database sessions.
///
/// This middleware runs after auth and expects TenantContext to already be extracted
/// by the router layer (via FromRequestParts).
pub async fn tenant_context_middleware(
    req: Request<Body>,
    next: Next,
) -> Response {
    // TenantContext is extracted by handlers via FromRequestParts, not here.
    // This middleware is a placeholder for future cross-cutting concerns like:
    // - Logging tenant_id with all requests
    // - Enforcing tenant-specific rate limits
    // - Auditing tenant operations
    next.run(req).await
}

/// Extract tenant_id from request for use in database queries.
/// Returns None if tenant context is not available (fail-closed).
pub fn get_tenant_id_from_request(req: &Request<Body>) -> Option<Uuid> {
    // Tenant context would be set by upstream middleware
    // For now, this is a utility for future use
    None
}
