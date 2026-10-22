//! Admin endpoints for the operator audit log (#1518). Mounted under
//! `admin_only_routes`, so `admin_auth` gates both - there is no
//! unauthenticated read or write path. Writes happen only through
//! `crate::audit::dispatch`; these endpoints are read-only.

use crate::audit::{self, AuditQuery};
use crate::error::AppError;
use crate::ApiState;
use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};

/// `GET /admin/operator-audit` - filtered, keyset-paginated (`after_seq`) list.
pub async fn list_operator_audit(
    State(state): State<ApiState>,
    Query(q): Query<AuditQuery>,
) -> Result<impl IntoResponse, AppError> {
    let records = audit::list(&state.app_state.db, &q)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;
    let next_after_seq = records.last().map(|r| r.seq);
    Ok(Json(serde_json::json!({
        "count": records.len(),
        "data": records,
        "next_after_seq": next_after_seq,
    })))
}

/// `GET /admin/operator-audit/export` - the full chain as NDJSON, for periodic
/// internal-controls review. `X-Audit-Chain-Valid` reports verification and
/// `X-Audit-Chain-First-Invalid-Seq` names the first broken record, if any.
pub async fn export_operator_audit(State(state): State<ApiState>) -> Result<Response, AppError> {
    let (records, verified) = audit::export_all(&state.app_state.db)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    let mut body = String::new();
    for r in &records {
        body.push_str(&serde_json::to_string(r).map_err(|e| AppError::Internal(e.to_string()))?);
        body.push('\n');
    }

    let mut resp = (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/x-ndjson"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"operator_audit_log.ndjson\"",
            ),
        ],
        body,
    )
        .into_response();
    let headers = resp.headers_mut();
    headers.insert(
        "x-audit-chain-valid",
        if verified.is_ok() { "true" } else { "false" }
            .parse()
            .unwrap(),
    );
    if let Err(seq) = verified {
        tracing::error!(seq, "operator audit chain verification failed");
        headers.insert("x-audit-chain-first-invalid-seq", seq.into());
    }
    Ok(resp)
}
