use crate::error::AppError;
use crate::swap::disputes::{self, DisputeError, DisputeFilter, DisputeUpdate, NewDispute};
use crate::ApiState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use uuid::Uuid;

fn map_err(e: DisputeError) -> AppError {
    match e {
        DisputeError::NotFound => AppError::NotFound("dispute not found".into()),
        DisputeError::Db(e) => AppError::Database(e),
        other => AppError::BadRequest(other.to_string()),
    }
}

pub async fn create_dispute(
    State(state): State<ApiState>,
    tenant: crate::tenant::TenantContext,
    Json(body): Json<NewDispute>,
) -> Result<impl IntoResponse, AppError> {
    let pool = state.app_state.pool_manager.primary();
    // Orders are tenant-scoped via their transaction record; absent record
    // (failed to persist a fill) is allowed.
    let order_tenant: Option<Uuid> =
        sqlx::query_scalar("SELECT tenant_id FROM transactions WHERE id = $1 LIMIT 1")
            .bind(body.order_id)
            .fetch_optional(pool)
            .await
            .map_err(AppError::Database)?
            .flatten();
    let d = disputes::create(
        pool,
        tenant.tenant_id,
        order_tenant,
        body,
        &tenant.tenant_id.to_string(),
    )
    .await
    .map_err(map_err)?;
    Ok((StatusCode::CREATED, Json(d)))
}

pub async fn update_dispute(
    State(state): State<ApiState>,
    tenant: crate::tenant::TenantContext,
    Path(id): Path<Uuid>,
    Json(body): Json<DisputeUpdate>,
) -> Result<impl IntoResponse, AppError> {
    let pool = state.app_state.pool_manager.primary();
    let d = disputes::update(
        pool,
        tenant.tenant_id,
        id,
        body,
        &tenant.tenant_id.to_string(),
    )
    .await
    .map_err(map_err)?;
    Ok(Json(d))
}

pub async fn list_disputes(
    State(state): State<ApiState>,
    tenant: crate::tenant::TenantContext,
    Query(filter): Query<DisputeFilter>,
) -> Result<impl IntoResponse, AppError> {
    let pool = state.app_state.pool_manager.primary();
    Ok(Json(
        disputes::list(pool, tenant.tenant_id, &filter)
            .await
            .map_err(map_err)?,
    ))
}
