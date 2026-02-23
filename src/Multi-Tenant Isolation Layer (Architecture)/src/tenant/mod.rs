use axum::{
    async_trait,
    extract::{FromRequestParts, Path},
    http::{request::Parts, HeaderMap},
    RequestPartsExt,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{config::AppState, error::{AppError, Result}};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantConfig {
    pub tenant_id: Uuid,
    pub name: String,
    pub webhook_secret: String,
    pub stellar_account: String,
    pub rate_limit_per_minute: i32,
    pub is_active: bool,
}

#[derive(Debug, Clone)]
pub struct TenantContext {
    pub tenant_id: Uuid,
    pub config: TenantConfig,
}

impl TenantContext {
    pub fn new(tenant_id: Uuid, config: TenantConfig) -> Self {
        Self { tenant_id, config }
    }
}

#[async_trait]
impl FromRequestParts<AppState> for TenantContext {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self> {
        let tenant_id = resolve_tenant_id(parts, state).await?;
        
        let config = state
            .get_tenant_config(tenant_id)
            .await
            .ok_or(AppError::TenantNotFound)?;
        
        if !config.is_active {
            return Err(AppError::Unauthorized);
        }
        
        Ok(TenantContext::new(tenant_id, config))
    }
}

async fn resolve_tenant_id(parts: &mut Parts, state: &AppState) -> Result<Uuid> {
    if let Ok(Path(tenant_id)) = parts.extract::<Path<Uuid>>().await {
        return Ok(tenant_id);
    }
    
    let headers = &parts.headers;
    
    if let Some(api_key) = extract_api_key(headers) {
        return resolve_tenant_by_api_key(&state.pool, &api_key).await;
    }
    
    if let Some(tenant_id_str) = headers.get("X-Tenant-ID") {
        if let Ok(tenant_id) = tenant_id_str.to_str()
            .ok()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or(AppError::InvalidApiKey)
        {
            return Ok(tenant_id);
        }
    }
    
    Err(AppError::InvalidApiKey)
}

fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get("X-API-Key")
        .or_else(|| headers.get("Authorization"))
        .and_then(|v| v.to_str().ok())
        .map(|s| {
            if s.starts_with("Bearer ") {
                s.trim_start_matches("Bearer ").to_string()
            } else {
                s.to_string()
            }
        })
}

async fn resolve_tenant_by_api_key(pool: &sqlx::PgPool, api_key: &str) -> Result<Uuid> {
    let result = sqlx::query!(
        r#"
        SELECT tenant_id 
        FROM tenants 
        WHERE api_key = $1 AND is_active = true
        "#,
        api_key
    )
    .fetch_optional(pool)
    .await?;
    
    result
        .map(|r| r.tenant_id)
        .ok_or(AppError::InvalidApiKey)
}
