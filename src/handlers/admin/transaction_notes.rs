//! Transaction annotation/notes API handlers (Issue #1257)
//!
//! Provides endpoints for operations and support staff to document manual investigation
//! work on transactions through append-only, immutable notes.

use crate::db::queries::{self, CreateTransactionNoteRequest, PaginatedNotes};
use crate::error::AppError;
use crate::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Query parameters for note pagination
#[derive(Debug, Deserialize)]
pub struct NotesPageQuery {
    /// Maximum number of notes to return (default: 10, max: 100)
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// Cursor for pagination (ID of last item from previous page)
    pub cursor: Option<i64>,
}

fn default_limit() -> i64 {
    10
}

/// Response wrapper for paginated notes
#[derive(Debug, Serialize)]
pub struct NotesResponse {
    pub notes: Vec<NoteResponse>,
    pub pagination: PaginationInfo,
}

/// Individual note response
#[derive(Debug, Serialize)]
pub struct NoteResponse {
    pub id: i64,
    pub transaction_id: String,
    pub admin_principal: String,
    pub note_text: String,
    pub created_at: String,
}

/// Pagination metadata
#[derive(Debug, Serialize)]
pub struct PaginationInfo {
    pub has_more: bool,
    pub next_cursor: Option<i64>,
    pub limit: i64,
}

/// Create a note on a transaction
/// POST /admin/transactions/:id/notes
pub async fn create_transaction_note(
    State(state): State<AppState>,
    Path(transaction_id): Path<Uuid>,
    Json(req): Json<CreateTransactionNoteRequest>,
) -> Result<impl IntoResponse, AppError> {
    // Validate request
    if req.note_text.trim().is_empty() {
        return Err(AppError::BadRequest("note_text cannot be empty".to_string()));
    }

    if req.note_text.len() > 10_000 {
        return Err(AppError::BadRequest(
            "note_text exceeds maximum length of 10000 characters".to_string(),
        ));
    }

    // Get admin principal from context (from auth middleware)
    let admin_principal = state.tenant_id.clone(); // In production, use actual principal from auth headers

    let note = queries::add_transaction_note(
        &state.db_pool,
        transaction_id,
        &admin_principal,
        &req.note_text,
    )
    .await
    .map_err(|e| {
        tracing::error!("Failed to create transaction note: {}", e);
        AppError::InternalServerError("Failed to create note".to_string())
    })?;

    Ok((
        StatusCode::CREATED,
        Json(NoteResponse {
            id: note.id,
            transaction_id: note.transaction_id.to_string(),
            admin_principal: note.admin_principal,
            note_text: note.note_text,
            created_at: note.created_at.to_rfc3339(),
        }),
    ))
}

/// Get paginated notes for a transaction
/// GET /admin/transactions/:id/notes
pub async fn get_transaction_notes(
    State(state): State<AppState>,
    Path(transaction_id): Path<Uuid>,
    Query(pagination): Query<NotesPageQuery>,
) -> Result<impl IntoResponse, AppError> {
    // Validate pagination parameters
    let limit = pagination.limit.clamp(1, 100);

    let paginated_notes = queries::get_transaction_notes(
        &state.db_pool,
        transaction_id,
        limit,
        pagination.cursor,
    )
    .await
    .map_err(|e| {
        tracing::error!("Failed to fetch transaction notes: {}", e);
        AppError::InternalServerError("Failed to fetch notes".to_string())
    })?;

    let notes = paginated_notes
        .items
        .into_iter()
        .map(|note| NoteResponse {
            id: note.id,
            transaction_id: note.transaction_id.to_string(),
            admin_principal: note.admin_principal,
            note_text: note.note_text,
            created_at: note.created_at.to_rfc3339(),
        })
        .collect();

    Ok(Json(NotesResponse {
        notes,
        pagination: PaginationInfo {
            has_more: paginated_notes.has_more,
            next_cursor: paginated_notes.cursor,
            limit,
        },
    }))
}
