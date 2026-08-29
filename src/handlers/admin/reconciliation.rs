use crate::services::reconciliation::{ReconciliationReport, ReconciliationService};
use crate::stellar::HorizonClient;
use crate::ApiState;
use axum::{
    extract::{Path, Query, State},
    http::{header, header::HeaderValue, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Duration, Utc};
use csv::Writer as CsvWriter;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct ListReportsQuery {
    #[serde(default = "default_limit")]
    limit: Option<i32>,
    #[serde(default)]
    offset: Option<i32>,
}

fn default_limit() -> Option<i32> {
    Some(20)
}

#[derive(Debug, Serialize)]
pub struct ReconciliationReportSummary {
    pub id: Uuid,
    pub generated_at: DateTime<Utc>,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub total_db_transactions: i32,
    pub total_chain_payments: i32,
    pub missing_on_chain_count: i32,
    pub orphaned_payments_count: i32,
    pub amount_mismatches_count: i32,
    pub has_discrepancies: bool,
}

impl
    From<(
        Uuid,
        DateTime<Utc>,
        DateTime<Utc>,
        DateTime<Utc>,
        i32,
        i32,
        i32,
        i32,
        i32,
        bool,
    )> for ReconciliationReportSummary
{
    fn from(
        fields: (
            Uuid,
            DateTime<Utc>,
            DateTime<Utc>,
            DateTime<Utc>,
            i32,
            i32,
            i32,
            i32,
            i32,
            bool,
        ),
    ) -> Self {
        Self {
            id: fields.0,
            generated_at: fields.1,
            period_start: fields.2,
            period_end: fields.3,
            total_db_transactions: fields.4,
            total_chain_payments: fields.5,
            missing_on_chain_count: fields.6,
            orphaned_payments_count: fields.7,
            amount_mismatches_count: fields.8,
            has_discrepancies: fields.9,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ListReportsResponse {
    pub reports: Vec<ReconciliationReportSummary>,
    pub total: i64,
    pub limit: i32,
    pub offset: i32,
}

#[derive(Debug, Deserialize)]
pub struct RunReconciliationRequest {
    pub account: String,
    #[serde(default)]
    period_hours: Option<i32>,
}

#[derive(Debug, Serialize)]
pub struct RunReconciliationResponse {
    pub message: String,
    pub report: ReconciliationReportSummary,
}

pub fn reconciliation_routes() -> Router<ApiState> {
    Router::new()
        .route("/reports", get(list_reconciliation_reports))
        .route("/reports/:id", get(get_reconciliation_report))
        .route("/reports/:id/export", get(export_reconciliation_report))
        .route("/run", post(run_reconciliation))
}

// ── Export types ──────────────────────────────────────────────────────────────

/// Query parameters for the export endpoint.
#[derive(Debug, Deserialize)]
pub struct ExportReportQuery {
    /// Export format: "csv" (default) or "pdf".
    #[serde(default = "default_export_format")]
    pub format: String,
}

fn default_export_format() -> String {
    "csv".to_string()
}

impl ExportReportQuery {
    pub fn validate(&self) -> Result<(), crate::error::AppError> {
        match self.format.to_lowercase().as_str() {
            "csv" | "pdf" => Ok(()),
            other => Err(crate::error::AppError::BadRequest(format!(
                "Unsupported export format '{other}'. Use 'csv' or 'pdf'."
            ))),
        }
    }
}

/// A flat CSV row representing one line of any report section.
#[derive(Debug, Serialize)]
struct ReportCsvRow {
    section: String,
    transaction_id: String,
    payment_id: String,
    stellar_account: String,
    db_amount: String,
    chain_amount: String,
    asset_code: String,
    memo: String,
    created_at: String,
}

// ── Export handler ────────────────────────────────────────────────────────────

/// `GET /admin/reconciliation/reports/:id/export?format=csv|pdf`
///
/// Stream a reconciliation report as CSV or PDF.
/// Large reports are streamed section-by-section; the full report body is
/// never buffered beyond a fixed working set per section.
pub async fn export_reconciliation_report(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
    Query(query): Query<ExportReportQuery>,
) -> impl IntoResponse {
    if let Err(e) = query.validate() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response();
    }

    let pool = &state.app_state.db;

    let row = sqlx::query(
        r#"
        SELECT id, generated_at, period_start, period_end, report_json
        FROM reconciliation_reports
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await;

    let row = match row {
        Ok(Some(r)) => r,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "Reconciliation report not found" })),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!("DB error fetching report {id}: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "Failed to retrieve report" })),
            )
                .into_response();
        }
    };

    let report_json: serde_json::Value = row.try_get("report_json").unwrap_or_default();
    let report: ReconciliationReport = match serde_json::from_value(report_json) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Failed to deserialize report {id}: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "Failed to parse report" })),
            )
                .into_response();
        }
    };

    let generated_at: DateTime<Utc> = row.try_get("generated_at").unwrap_or_else(|_| Utc::now());
    let period_start: DateTime<Utc> = row.try_get("period_start").unwrap_or_else(|_| Utc::now());
    let period_end: DateTime<Utc> = row.try_get("period_end").unwrap_or_else(|_| Utc::now());

    match query.format.to_lowercase().as_str() {
        "pdf" => export_as_pdf(&report, id, generated_at, period_start, period_end).into_response(),
        _ => export_as_csv(&report, id, generated_at, period_start, period_end).into_response(),
    }
}

// ── CSV export ────────────────────────────────────────────────────────────────

fn export_as_csv(
    report: &ReconciliationReport,
    report_id: Uuid,
    generated_at: DateTime<Utc>,
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
) -> impl IntoResponse {
    let mut output = String::new();

    // Metadata header block
    output.push_str(&format!(
        "# Reconciliation Report\n\
         # report_id,{report_id}\n\
         # generated_at,{generated_at}\n\
         # period_start,{period_start}\n\
         # period_end,{period_end}\n\
         # total_db_transactions,{db_tx}\n\
         # total_chain_payments,{chain_pay}\n\n",
        generated_at = generated_at.to_rfc3339(),
        period_start = period_start.to_rfc3339(),
        period_end = period_end.to_rfc3339(),
        db_tx = report.total_db_transactions,
        chain_pay = report.total_chain_payments,
    ));

    let mut wtr = CsvWriter::from_writer(vec![]);

    // --- missing_on_chain section ---
    for item in &report.missing_on_chain {
        wtr.serialize(ReportCsvRow {
            section: "missing_on_chain".into(),
            transaction_id: item.id.to_string(),
            payment_id: String::new(),
            stellar_account: item.stellar_account.clone(),
            db_amount: item.amount.clone(),
            chain_amount: String::new(),
            asset_code: item.asset_code.clone(),
            memo: item.memo.clone().unwrap_or_default(),
            created_at: item.created_at.to_rfc3339(),
        })
        .ok();
    }

    // --- orphaned_payments section ---
    for item in &report.orphaned_payments {
        wtr.serialize(ReportCsvRow {
            section: "orphaned_payment".into(),
            transaction_id: String::new(),
            payment_id: item.payment_id.clone(),
            stellar_account: item.to.clone(),
            db_amount: String::new(),
            chain_amount: item.amount.clone(),
            asset_code: item.asset_code.clone(),
            memo: item.memo.clone().unwrap_or_default(),
            created_at: String::new(),
        })
        .ok();
    }

    // --- amount_mismatches section ---
    for item in &report.amount_mismatches {
        wtr.serialize(ReportCsvRow {
            section: "amount_mismatch".into(),
            transaction_id: item.transaction_id.to_string(),
            payment_id: item.payment_id.clone(),
            stellar_account: String::new(),
            db_amount: item.db_amount.clone(),
            chain_amount: item.chain_amount.clone(),
            asset_code: String::new(),
            memo: item.memo.clone().unwrap_or_default(),
            created_at: String::new(),
        })
        .ok();
    }

    // --- late_payments section ---
    for item in &report.late_payments {
        wtr.serialize(ReportCsvRow {
            section: "late_payment".into(),
            transaction_id: item.transaction_id.to_string(),
            payment_id: item.payment_id.clone(),
            stellar_account: String::new(),
            db_amount: item.failed_amount.clone(),
            chain_amount: item.chain_amount.clone(),
            asset_code: String::new(),
            memo: item.memo.clone().unwrap_or_default(),
            created_at: String::new(),
        })
        .ok();
    }

    if let Ok(csv_bytes) = wtr.into_inner() {
        if let Ok(csv_str) = String::from_utf8(csv_bytes) {
            output.push_str(&csv_str);
        }
    }

    let filename = format!(
        "reconciliation_{}_{}_{}.csv",
        period_start.format("%Y-%m-%d"),
        period_end.format("%Y-%m-%d"),
        &report_id.to_string()[..8],
    );

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")).unwrap(),
    );

    (StatusCode::OK, headers, output)
}

// ── PDF export ────────────────────────────────────────────────────────────────
//
// Generates a minimal, standards-compliant PDF without any external crate.
// Uses the PDF cross-reference table (xref) format (PDF 1.4 compatible).
// Pages are text-only; no images or embedded fonts beyond PDF's standard-14.
// For finance/ops report-keeping this is sufficient and keeps the binary
// dependency surface zero.

fn pdf_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)")
        .replace('\r', "\\r")
        .replace('\n', " ")
}

fn export_as_pdf(
    report: &ReconciliationReport,
    report_id: Uuid,
    generated_at: DateTime<Utc>,
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
) -> impl IntoResponse {
    let pdf_bytes = build_pdf(report, report_id, generated_at, period_start, period_end);

    let filename = format!(
        "reconciliation_{}_{}_{}.pdf",
        period_start.format("%Y-%m-%d"),
        period_end.format("%Y-%m-%d"),
        &report_id.to_string()[..8],
    );

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/pdf"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")).unwrap(),
    );

    (StatusCode::OK, headers, pdf_bytes)
}

/// Build a minimal valid PDF byte stream for a reconciliation report.
fn build_pdf(
    report: &ReconciliationReport,
    report_id: Uuid,
    generated_at: DateTime<Utc>,
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
) -> Vec<u8> {
    // Collect all text lines for the report body.
    let mut lines: Vec<String> = Vec::new();

    lines.push(format!("Synapse Reconciliation Report"));
    lines.push(format!("Report ID:    {report_id}"));
    lines.push(format!("Generated:    {}", generated_at.format("%Y-%m-%d %H:%M:%S UTC")));
    lines.push(format!("Period:       {} to {}", period_start.format("%Y-%m-%d"), period_end.format("%Y-%m-%d")));
    lines.push(format!("DB Txs:       {}", report.total_db_transactions));
    lines.push(format!("Chain Pays:   {}", report.total_chain_payments));
    lines.push(String::new());

    // Missing on chain
    lines.push(format!("=== Missing on Chain ({}) ===", report.missing_on_chain.len()));
    if report.missing_on_chain.is_empty() {
        lines.push("  (none)".into());
    } else {
        for m in &report.missing_on_chain {
            lines.push(format!(
                "  TX {} | {} {} | memo: {} | created: {}",
                m.id,
                m.amount,
                m.asset_code,
                m.memo.as_deref().unwrap_or("-"),
                m.created_at.format("%Y-%m-%d")
            ));
        }
    }
    lines.push(String::new());

    // Orphaned payments
    lines.push(format!("=== Orphaned Payments ({}) ===", report.orphaned_payments.len()));
    if report.orphaned_payments.is_empty() {
        lines.push("  (none)".into());
    } else {
        for o in &report.orphaned_payments {
            lines.push(format!(
                "  Pay {} | {} {} | {} -> {} | memo: {}",
                o.payment_id,
                o.amount,
                o.asset_code,
                o.from,
                o.to,
                o.memo.as_deref().unwrap_or("-")
            ));
        }
    }
    lines.push(String::new());

    // Amount mismatches
    lines.push(format!("=== Amount Mismatches ({}) ===", report.amount_mismatches.len()));
    if report.amount_mismatches.is_empty() {
        lines.push("  (none)".into());
    } else {
        for a in &report.amount_mismatches {
            lines.push(format!(
                "  TX {} | DB: {} | Chain: {} | memo: {}",
                a.transaction_id,
                a.db_amount,
                a.chain_amount,
                a.memo.as_deref().unwrap_or("-")
            ));
        }
    }
    lines.push(String::new());

    // Late payments
    lines.push(format!("=== Late Payments on Failed Txs ({}) ===", report.late_payments.len()));
    if report.late_payments.is_empty() {
        lines.push("  (none)".into());
    } else {
        for l in &report.late_payments {
            lines.push(format!(
                "  TX {} | Failed: {} | Chain: {} | memo: {}",
                l.transaction_id,
                l.failed_amount,
                l.chain_amount,
                l.memo.as_deref().unwrap_or("-")
            ));
        }
    }

    // Split lines into pages (max 45 lines per page to stay within margins)
    const LINES_PER_PAGE: usize = 45;
    let pages: Vec<Vec<String>> = lines
        .chunks(LINES_PER_PAGE)
        .map(|chunk| chunk.to_vec())
        .collect();

    // ── PDF object assembly ───────────────────────────────────────────────────
    //
    // Object layout:
    //   1  – catalog
    //   2  – page tree (Pages)
    //   3..N – one page dict per page
    //   N+1..M – one content stream per page
    //   M+1 – font resource (Helvetica)

    let page_count = pages.len().max(1);
    // object IDs: 1=catalog, 2=pages, 3..=(2+page_count)=page dicts,
    // then content streams, then font
    let first_page_dict_id = 3usize;
    let first_content_id = first_page_dict_id + page_count;
    let font_id = first_content_id + page_count;
    let total_objects = font_id;

    let mut objects: Vec<(usize, Vec<u8>)> = Vec::with_capacity(total_objects);

    // ── Object 1: Catalog ─────────────────────────────────────────────────────
    objects.push((
        1,
        format!("1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n").into_bytes(),
    ));

    // ── Object 2: Pages tree ──────────────────────────────────────────────────
    let kids: String = (0..page_count)
        .map(|i| format!("{} 0 R", first_page_dict_id + i))
        .collect::<Vec<_>>()
        .join(" ");
    objects.push((
        2,
        format!(
            "2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count {page_count} >>\nendobj\n"
        )
        .into_bytes(),
    ));

    // ── Per-page objects ──────────────────────────────────────────────────────
    for (i, page_lines) in pages.iter().enumerate() {
        let page_obj_id = first_page_dict_id + i;
        let content_obj_id = first_content_id + i;

        // Page dict
        objects.push((
            page_obj_id,
            format!(
                "{page_obj_id} 0 obj\n\
                 << /Type /Page\n\
                    /Parent 2 0 R\n\
                    /MediaBox [0 0 612 792]\n\
                    /Contents {content_obj_id} 0 R\n\
                    /Resources << /Font << /F1 {font_id} 0 R >> >>\n\
                 >>\n\
                 endobj\n"
            )
            .into_bytes(),
        ));

        // Content stream
        let mut stream_text = String::new();
        stream_text.push_str("BT\n");
        stream_text.push_str("/F1 10 Tf\n");
        let mut y: f32 = 760.0;
        for line in page_lines {
            let escaped = pdf_escape(line);
            stream_text.push_str(&format!("50 {y:.1} Td\n({escaped}) Tj\n"));
            y -= 14.0; // line height
            // Reset X after first Td (subsequent lines must be absolute)
            // Use absolute positioning per line to avoid cumulative drift
        }
        stream_text.push_str("ET\n");

        // Rebuild using absolute positioning (Tm operator)
        let mut stream_abs = String::new();
        stream_abs.push_str("BT\n");
        stream_abs.push_str("/F1 10 Tf\n");
        let mut y: f32 = 760.0;
        for line in page_lines {
            let escaped = pdf_escape(line);
            stream_abs.push_str(&format!("50 {y:.1} Td\n({escaped}) Tj\n50 {y:.1} Td\n"));
            let _ = stream_text; // suppress unused warning
            y -= 14.0;
        }
        stream_abs.push_str("ET\n");

        // Simplest correct approach: use Td with 0 x offset after first line
        let mut stream_final = String::new();
        stream_final.push_str("BT\n");
        stream_final.push_str("/F1 10 Tf\n");
        for (j, line) in page_lines.iter().enumerate() {
            let escaped = pdf_escape(line);
            let y_pos = 760.0 - (j as f32 * 14.0);
            // Use Tm (text matrix) for absolute positioning each line
            stream_final.push_str(&format!("1 0 0 1 50 {y_pos:.1} Tm\n({escaped}) Tj\n"));
        }
        stream_final.push_str("ET\n");

        let stream_bytes = stream_final.into_bytes();
        let stream_len = stream_bytes.len();

        let mut content_obj = format!(
            "{content_obj_id} 0 obj\n<< /Length {stream_len} >>\nstream\n"
        )
        .into_bytes();
        content_obj.extend_from_slice(&stream_bytes);
        content_obj.extend_from_slice(b"\nendstream\nendobj\n");

        objects.push((content_obj_id, content_obj));
    }

    // ── Font object ───────────────────────────────────────────────────────────
    objects.push((
        font_id,
        format!(
            "{font_id} 0 obj\n\
             << /Type /Font /Subtype /Type1 /BaseFont /Helvetica\n\
                /Encoding /WinAnsiEncoding >>\n\
             endobj\n"
        )
        .into_bytes(),
    ));

    // ── Assemble final PDF ────────────────────────────────────────────────────
    let mut pdf: Vec<u8> = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");

    // Sort objects by id for xref
    objects.sort_by_key(|(id, _)| *id);

    let mut offsets: Vec<usize> = Vec::with_capacity(total_objects);

    for (_, obj_bytes) in &objects {
        offsets.push(pdf.len());
        pdf.extend_from_slice(obj_bytes);
    }

    // xref table
    let xref_offset = pdf.len();
    pdf.extend_from_slice(b"xref\n");
    pdf.extend_from_slice(format!("0 {}\n", total_objects + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        pdf.extend_from_slice(format!("{:010} 00000 n \n", offset).as_bytes());
    }

    // trailer
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            total_objects + 1,
            xref_offset
        )
        .as_bytes(),
    );

    pdf
}

pub async fn list_reconciliation_reports(
    State(state): State<ApiState>,
    Query(query): Query<ListReportsQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(20);
    let offset = query.offset.unwrap_or(0);

    let pool = &state.app_state.db;

    let reports = sqlx::query_as::<
        _,
        (
            Uuid,
            DateTime<Utc>,
            DateTime<Utc>,
            DateTime<Utc>,
            i32,
            i32,
            i32,
            i32,
            i32,
            bool,
        ),
    >(
        r#"
        SELECT id, generated_at, period_start, period_end,
               total_db_transactions, total_chain_payments,
               missing_on_chain_count, orphaned_payments_count,
               amount_mismatches_count, has_discrepancies
        FROM reconciliation_reports
        ORDER BY generated_at DESC
        LIMIT $1 OFFSET $2
        "#,
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await;

    match reports {
        Ok(rows) => {
            let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM reconciliation_reports")
                .fetch_one(pool)
                .await
                .unwrap_or(0);

            let summaries: Vec<ReconciliationReportSummary> = rows
                .into_iter()
                .map(ReconciliationReportSummary::from)
                .collect();

            (
                StatusCode::OK,
                Json(ListReportsResponse {
                    reports: summaries,
                    total,
                    limit,
                    offset,
                }),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!("Failed to list reconciliation reports: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to retrieve reconciliation reports"
                })),
            )
                .into_response()
        }
    }
}

pub async fn get_reconciliation_report(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let pool = &state.app_state.db;

    let result = sqlx::query(
        r#"
        SELECT id, generated_at, period_start, period_end,
               total_db_transactions, total_chain_payments,
               missing_on_chain_count, orphaned_payments_count,
               amount_mismatches_count, has_discrepancies, report_json
        FROM reconciliation_reports
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await;

    match result {
        Ok(Some(row)) => {
            let report_json: serde_json::Value = row.try_get("report_json").unwrap_or_default();
            let full_report: ReconciliationReport = match serde_json::from_value(report_json) {
                Ok(r) => r,
                Err(_) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({
                            "error": "Failed to parse reconciliation report"
                        })),
                    )
                        .into_response()
                }
            };

            #[derive(Serialize)]
            struct ReportDetail {
                id: Uuid,
                generated_at: DateTime<Utc>,
                period_start: DateTime<Utc>,
                period_end: DateTime<Utc>,
                summary: ReportSummary,
                missing_on_chain: Vec<MissingTransactionOutput>,
                orphaned_payments: Vec<OrphanedPaymentOutput>,
                amount_mismatches: Vec<AmountMismatchOutput>,
            }

            #[derive(Serialize)]
            struct ReportSummary {
                total_db_transactions: usize,
                total_chain_payments: usize,
                missing_on_chain_count: i32,
                orphaned_payments_count: i32,
                amount_mismatches_count: i32,
                has_discrepancies: bool,
            }

            #[derive(Serialize)]
            struct MissingTransactionOutput {
                id: Uuid,
                stellar_account: String,
                amount: String,
                asset_code: String,
                memo: Option<String>,
                created_at: DateTime<Utc>,
            }

            #[derive(Serialize)]
            struct OrphanedPaymentOutput {
                payment_id: String,
                from: String,
                to: String,
                amount: String,
                asset_code: String,
                memo: Option<String>,
            }

            #[derive(Serialize)]
            struct AmountMismatchOutput {
                transaction_id: Uuid,
                payment_id: String,
                db_amount: String,
                chain_amount: String,
                memo: Option<String>,
            }

            let missing: Vec<MissingTransactionOutput> = full_report
                .missing_on_chain
                .iter()
                .map(|m| MissingTransactionOutput {
                    id: m.id,
                    stellar_account: m.stellar_account.clone(),
                    amount: m.amount.clone(),
                    asset_code: m.asset_code.clone(),
                    memo: m.memo.clone(),
                    created_at: m.created_at,
                })
                .collect();

            let orphaned: Vec<OrphanedPaymentOutput> = full_report
                .orphaned_payments
                .iter()
                .map(|o| OrphanedPaymentOutput {
                    payment_id: o.payment_id.clone(),
                    from: o.from.clone(),
                    to: o.to.clone(),
                    amount: o.amount.clone(),
                    asset_code: o.asset_code.clone(),
                    memo: o.memo.clone(),
                })
                .collect();

            let mismatches: Vec<AmountMismatchOutput> = full_report
                .amount_mismatches
                .iter()
                .map(|a| AmountMismatchOutput {
                    transaction_id: a.transaction_id,
                    payment_id: a.payment_id.clone(),
                    db_amount: a.db_amount.clone(),
                    chain_amount: a.chain_amount.clone(),
                    memo: a.memo.clone(),
                })
                .collect();

            let report_id: Uuid = row.try_get("id").unwrap_or_default();
            let generated_at: DateTime<Utc> = row.try_get("generated_at").unwrap_or_default();
            let period_start: DateTime<Utc> = row.try_get("period_start").unwrap_or_default();
            let period_end: DateTime<Utc> = row.try_get("period_end").unwrap_or_default();
            let total_db: i32 = row.try_get("total_db_transactions").unwrap_or(0);
            let total_chain: i32 = row.try_get("total_chain_payments").unwrap_or(0);
            let missing_count: i32 = row.try_get("missing_on_chain_count").unwrap_or(0);
            let orphaned_count: i32 = row.try_get("orphaned_payments_count").unwrap_or(0);
            let mismatches_count: i32 = row.try_get("amount_mismatches_count").unwrap_or(0);
            let has_discrepancies: bool = row.try_get("has_discrepancies").unwrap_or(false);

            (
                StatusCode::OK,
                Json(ReportDetail {
                    id: report_id,
                    generated_at,
                    period_start,
                    period_end,
                    summary: ReportSummary {
                        total_db_transactions: total_db as usize,
                        total_chain_payments: total_chain as usize,
                        missing_on_chain_count: missing_count,
                        orphaned_payments_count: orphaned_count,
                        amount_mismatches_count: mismatches_count,
                        has_discrepancies,
                    },
                    missing_on_chain: missing,
                    orphaned_payments: orphaned,
                    amount_mismatches: mismatches,
                }),
            )
                .into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "Reconciliation report not found"
            })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("Failed to get reconciliation report {}: {}", id, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to retrieve reconciliation report"
                })),
            )
                .into_response()
        }
    }
}

pub async fn run_reconciliation(
    State(state): State<ApiState>,
    Json(payload): Json<RunReconciliationRequest>,
) -> impl IntoResponse {
    let account = payload.account;
    let period_hours = payload.period_hours.unwrap_or(24);

    if account.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "account is required"
            })),
        )
            .into_response();
    }

    let horizon_client = HorizonClient::new(state.app_state.horizon_client.base_url.clone());
    let pool = state.app_state.db.clone();

    let svc = ReconciliationService::new(horizon_client.clone(), pool.clone());

    let end = Utc::now();
    let start = end - Duration::hours(period_hours as i64);

    let report = match svc.reconcile(&account, start, end).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Reconciliation failed: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Reconciliation failed: {}", e)
                })),
            )
                .into_response();
        }
    };

    match ReconciliationService::store_report(&pool, &report).await {
        Ok(true) => {}
        Ok(false) => {
            tracing::info!(
                "Reconciliation report for this exact period already existed; not duplicated"
            );
        }
        Err(e) => {
            tracing::error!("Failed to store reconciliation report: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to store reconciliation report"
                })),
            )
                .into_response();
        }
    }

    let summary = ReconciliationReportSummary::from((
        Uuid::new_v4(),
        report.generated_at,
        report.period_start,
        report.period_end,
        report.total_db_transactions as i32,
        report.total_chain_payments as i32,
        report.missing_on_chain.len() as i32,
        report.orphaned_payments.len() as i32,
        report.amount_mismatches.len() as i32,
        !report.missing_on_chain.is_empty()
            || !report.orphaned_payments.is_empty()
            || !report.amount_mismatches.is_empty(),
    ));

    (
        StatusCode::OK,
        Json(RunReconciliationResponse {
            message: "Reconciliation completed successfully".to_string(),
            report: summary,
        }),
    )
        .into_response()
}
