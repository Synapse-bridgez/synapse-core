//! Admin endpoints for the dependency health scorecard (#1334).
//!
//! - `GET /admin/dependencies/scorecard?windows=7,30,90` — JSON scorecard
//! - `GET /admin/dependencies/scorecard/raw?dependency=&since=&until=&format=json|csv`
//!   — raw 5-minute rollup rows for capacity-planning / postmortem tooling
//! - `GET /admin/dependencies/scorecard/dashboard` — server-rendered HTML view
//!
//! All sit behind `admin_auth`. See docs/dependency-scorecard.md.

use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::error::AppError;
use crate::services::dependency_scorecard::{
    self as scorecard, Dependency, Grade, Scorecard, ScorecardEntry, MAX_WINDOW_DAYS,
    RETENTION_DAYS,
};
use crate::ApiState;

#[derive(Debug, Deserialize)]
pub struct ScorecardQuery {
    pub windows: Option<String>,
}

pub async fn get_scorecard(
    State(state): State<ApiState>,
    Query(q): Query<ScorecardQuery>,
) -> Result<Json<Scorecard>, AppError> {
    let windows = scorecard::parse_windows(q.windows.as_deref()).map_err(AppError::BadRequest)?;
    let card = scorecard::load_scorecard(&state.app_state.db, Utc::now(), &windows).await?;
    Ok(Json(card))
}

#[derive(Debug, Deserialize)]
pub struct RawQuery {
    pub dependency: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub format: Option<String>,
}

/// Validated raw-export range: defaults to the last 24h, at most
/// `MAX_WINDOW_DAYS` wide, never older than retention.
pub fn raw_range(
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<(DateTime<Utc>, DateTime<Utc>), String> {
    let until = until.unwrap_or(now).min(now);
    let since = since.unwrap_or(until - chrono::Duration::days(1));
    if since >= until {
        return Err("`since` must be before `until`".to_string());
    }
    if until - since > chrono::Duration::days(MAX_WINDOW_DAYS as i64) {
        return Err(format!("range may span at most {MAX_WINDOW_DAYS} days"));
    }
    let oldest = now - chrono::Duration::days(RETENTION_DAYS);
    Ok((since.max(oldest), until))
}

pub async fn get_raw_rollups(
    State(state): State<ApiState>,
    Query(q): Query<RawQuery>,
) -> Result<Response, AppError> {
    let dependency = match q.dependency.as_deref() {
        None | Some("") => None,
        Some(d) => Some(
            Dependency::parse(d)
                .ok_or_else(|| AppError::BadRequest(format!("unknown dependency '{d}'")))?,
        ),
    };
    let (since, until) = raw_range(q.since, q.until, Utc::now()).map_err(AppError::BadRequest)?;
    let rows = scorecard::load_rollups(&state.app_state.db, since, until, dependency).await?;
    Ok(match q.format.as_deref() {
        Some("csv") => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/csv; charset=utf-8")],
            scorecard::rollups_to_csv(&rows),
        )
            .into_response(),
        None | Some("json") => Json(serde_json::json!({
            "since": since,
            "until": until,
            "rollup_period_minutes": scorecard::ROLLUP_PERIOD_MINUTES,
            "latency_bounds_ms": scorecard::LATENCY_BOUNDS_MS,
            "rows": rows,
        }))
        .into_response(),
        Some(other) => {
            return Err(AppError::BadRequest(format!(
                "unknown format '{other}' (expected json or csv)"
            )))
        }
    })
}

pub async fn get_dashboard(State(state): State<ApiState>) -> Result<Html<String>, AppError> {
    let card = scorecard::load_scorecard(
        &state.app_state.db,
        Utc::now(),
        &scorecard::DEFAULT_WINDOWS_DAYS,
    )
    .await?;
    Ok(Html(render_dashboard(&card)))
}

fn pct(v: Option<f64>, digits: usize) -> String {
    v.map(|v| format!("{:.*}%", digits, v * 100.0))
        .unwrap_or_else(|| "—".to_string())
}

fn msf(v: Option<f64>) -> String {
    v.map(|v| format!("{v:.1} ms"))
        .unwrap_or_else(|| "—".to_string())
}

fn grade_label(g: Grade) -> (&'static str, &'static str) {
    match g {
        Grade::Healthy => ("healthy", "Healthy"),
        Grade::Watch => ("watch", "Watch"),
        Grade::NeedsInvestment => ("invest", "Needs investment"),
        Grade::InsufficientData => ("nodata", "Insufficient data"),
    }
}

fn row(e: &ScorecardEntry) -> String {
    let (class, label) = grade_label(e.grade);
    format!(
        "<tr><td>{dep}</td><td>{w}d</td><td><span class=\"g {class}\">{label}</span></td>\
         <td>{up}</td><td>{err}</td><td>{part}</td><td>{p50}</td><td>{p95}</td><td>{p99}</td>\
         <td>{calls}</td><td>{trans}</td><td>{cov}</td></tr>",
        dep = e.dependency.as_str(),
        w = e.window_days,
        up = pct(e.uptime_ratio, 3),
        err = pct(e.error_rate, 2),
        part = pct(e.partition_error_rate, 2),
        p50 = msf(e.latency_p50_ms),
        p95 = msf(e.latency_p95_ms),
        p99 = msf(e.latency_p99_ms),
        calls = e.calls,
        trans = e.circuit_open_transitions,
        cov = pct(Some(e.coverage_ratio), 0),
    )
}

/// Self-contained HTML (no external assets). Every interpolated value is a
/// number or a fixed enum string, so no escaping is needed.
pub fn render_dashboard(card: &Scorecard) -> String {
    let rows: String = card.entries.iter().map(row).collect();
    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Dependency Scorecard</title>
<style>
:root {{ --bg:#fff; --fg:#1b1f24; --muted:#5b6470; --line:#e3e6ea; --ok:#1a7f37; --watch:#9a6700; --bad:#cf222e; }}
@media (prefers-color-scheme: dark) {{ :root {{ --bg:#0d1117; --fg:#e6edf3; --muted:#8b949e; --line:#30363d; --ok:#3fb950; --watch:#d29922; --bad:#f85149; }} }}
body {{ background:var(--bg); color:var(--fg); font:14px/1.45 system-ui,sans-serif; margin:0; padding:24px 16px; }}
h1 {{ font-size:20px; margin:0 0 4px; }} p {{ color:var(--muted); margin:0 0 16px; }}
.wrap {{ overflow-x:auto; }} table {{ border-collapse:collapse; width:100%; font-variant-numeric:tabular-nums; }}
th,td {{ text-align:right; padding:6px 10px; border-bottom:1px solid var(--line); white-space:nowrap; }}
th:nth-child(-n+3),td:nth-child(-n+3) {{ text-align:left; }} th {{ color:var(--muted); font-weight:600; }}
.g {{ font-weight:600; }} .healthy {{ color:var(--ok); }} .watch {{ color:var(--watch); }} .invest {{ color:var(--bad); }} .nodata {{ color:var(--muted); }}
</style></head><body>
<h1>Dependency health scorecard</h1>
<p>Generated {generated}. Error rate counts only failures attributed to the dependency; failures during our own network partitions are shown separately. Raw data: <code>/admin/dependencies/scorecard/raw</code>.</p>
<div class="wrap"><table>
<thead><tr><th>Dependency</th><th>Window</th><th>Grade</th><th>Uptime</th><th>Error rate</th><th>Partition err.</th><th>p50</th><th>p95</th><th>p99</th><th>Calls</th><th>Breaker opens</th><th>Coverage</th></tr></thead>
<tbody>{rows}</tbody></table></div>
</body></html>"#,
        generated = card.generated_at.to_rfc3339(),
        rows = rows
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 1, 12, 0, 0).unwrap()
    }

    #[test]
    fn raw_range_defaults_to_last_day() {
        let (since, until) = raw_range(None, None, now()).unwrap();
        assert_eq!(until, now());
        assert_eq!(until - since, chrono::Duration::days(1));
    }

    #[test]
    fn raw_range_validates_order_width_and_future() {
        let n = now();
        assert!(raw_range(Some(n), Some(n - chrono::Duration::hours(1)), n).is_err());
        assert!(raw_range(Some(n - chrono::Duration::days(91)), Some(n), n).is_err());
        let (_, until) = raw_range(None, Some(n + chrono::Duration::days(3)), n).unwrap();
        assert_eq!(until, n, "future `until` is clamped to now");
    }

    #[test]
    fn dashboard_renders_every_entry() {
        let card = scorecard::build_scorecard(&[], now(), &[7, 30, 90]);
        let html = render_dashboard(&card);
        assert!(html.starts_with("<!doctype html>"));
        assert_eq!(html.matches("<tr><td>").count(), 12);
        for dep in Dependency::ALL {
            assert!(html.contains(dep.as_str()));
        }
        assert!(html.contains("Insufficient data"));
    }

    #[test]
    fn formatting_helpers() {
        assert_eq!(pct(Some(0.99912), 3), "99.912%");
        assert_eq!(pct(None, 2), "—");
        assert_eq!(msf(Some(12.345)), "12.3 ms");
        assert_eq!(grade_label(Grade::Watch).0, "watch");
    }
}
