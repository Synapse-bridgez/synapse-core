use anyhow::{bail, Context, Result};
use chrono::{Duration, Utc};
use clap::Args;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::PathBuf,
    process::Command as ProcessCommand,
};

const SECONDS_PER_DAY: f64 = 86_400.0;
const RESIDUAL_BAND_Z: f64 = 1.96;
const DEFAULT_POOL_QUERY: &str = concat!(
    "max(db_pool_active_connections / ",
    "(db_pool_active_connections + db_pool_idle_connections))"
);

#[derive(Args)]
pub struct CapacityForecastArgs {
    /// Prometheus base URL (also read from PROMETHEUS_URL).
    #[arg(long, env = "PROMETHEUS_URL")]
    prometheus_url: Option<String>,

    /// Optional Prometheus bearer token (also read from PROMETHEUS_BEARER_TOKEN).
    #[arg(long, env = "PROMETHEUS_BEARER_TOKEN", hide_env_values = true)]
    prometheus_token: Option<String>,

    /// Number of historical days to fit.
    #[arg(long, default_value_t = 90)]
    window_days: i64,

    /// Prometheus range-query step in seconds.
    #[arg(long, default_value_t = 86_400)]
    step_secs: i64,

    /// Pool saturation threshold as a ratio (0..=1).
    #[arg(long, default_value_t = 0.8)]
    pool_threshold: f64,

    /// Maximum database storage in bytes (also read from DATABASE_STORAGE_CAPACITY_BYTES).
    #[arg(long, env = "DATABASE_STORAGE_CAPACITY_BYTES")]
    storage_capacity_bytes: Option<String>,

    /// Request throughput ceiling in requests/second from load testing.
    #[arg(long, env = "THROUGHPUT_CAPACITY_RPS", default_value_t = 1_000.0)]
    throughput_capacity_rps: f64,

    /// Prometheus query for the highest database-pool saturation ratio.
    #[arg(long, default_value = DEFAULT_POOL_QUERY)]
    pool_query: String,

    /// Prometheus query for total PostgreSQL storage in bytes.
    #[arg(long, default_value = "sum(pg_database_size_bytes)")]
    storage_query: String,

    /// Prometheus query for aggregate request throughput.
    #[arg(
        long,
        default_value = "sum(rate(http_request_duration_ms_count[5m]))"
    )]
    throughput_query: String,

    /// Directory for capacity-forecast.md and capacity-forecast.json.
    #[arg(long, default_value = "capacity-forecast")]
    out_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct DataPoint {
    pub timestamp: i64,
    pub value: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricForecast {
    pub name: String,
    pub unit: String,
    pub threshold: Option<f64>,
    pub sample_count: usize,
    pub first_sample_at: Option<i64>,
    pub last_sample_at: Option<i64>,
    pub current_estimate: Option<f64>,
    pub trend_per_day: Option<f64>,
    pub r_squared: Option<f64>,
    pub projected_days: Option<f64>,
    pub interval_95_days: Option<(f64, f64)>,
    pub status: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ForecastReport {
    pub generated_at: String,
    pub window_days: i64,
    pub uncertainty_method: String,
    pub forecasts: Vec<MetricForecast>,
}

fn parse_storage_capacity(value: Option<&str>) -> Result<Option<f64>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let capacity = value
        .parse::<f64>()
        .context("DATABASE_STORAGE_CAPACITY_BYTES must be a number")?;
    if !capacity.is_finite() || capacity <= 0.0 {
        bail!("DATABASE_STORAGE_CAPACITY_BYTES must be a positive finite number");
    }
    Ok(Some(capacity))
}

#[derive(Debug, Deserialize)]
struct PrometheusResponse {
    status: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    data: Option<PrometheusData>,
}

#[derive(Debug, Deserialize)]
struct PrometheusData {
    #[serde(default)]
    result: Vec<PrometheusSeries>,
}

#[derive(Debug, Deserialize)]
struct PrometheusSeries {
    #[serde(default)]
    values: Vec<(f64, String)>,
}

/// Fit an ordinary least-squares linear trend and estimate threshold-crossing time.
/// The interval translates a 1.96-RMSE residual band into earliest/latest crossing times.
pub fn forecast(
    name: &str,
    unit: &str,
    points: &[DataPoint],
    threshold: Option<f64>,
) -> MetricForecast {
    let mut points: Vec<DataPoint> = points
        .iter()
        .copied()
        .filter(|point| point.value.is_finite())
        .collect();
    points.sort_by_key(|point| point.timestamp);

    let first_sample_at = points.first().map(|point| point.timestamp);
    let last_sample_at = points.last().map(|point| point.timestamp);
    let empty = |status: &str, detail: String| MetricForecast {
        name: name.to_owned(),
        unit: unit.to_owned(),
        threshold,
        sample_count: points.len(),
        first_sample_at,
        last_sample_at,
        current_estimate: points.last().map(|point| point.value),
        trend_per_day: None,
        r_squared: None,
        projected_days: None,
        interval_95_days: None,
        status: status.to_owned(),
        detail: Some(detail),
    };

    let Some(threshold) = threshold else {
        return empty("unconfigured", "Set a capacity threshold to forecast this metric.".into());
    };
    if !threshold.is_finite() {
        return empty("invalid", "Capacity threshold must be finite.".into());
    }
    if points.len() < 3 {
        return empty("insufficient_data", "At least 3 finite samples are required.".into());
    }

    let origin = points[0].timestamp;
    let xs: Vec<f64> = points
        .iter()
        .map(|point| (point.timestamp - origin) as f64 / SECONDS_PER_DAY)
        .collect();
    let ys: Vec<f64> = points.iter().map(|point| point.value).collect();
    let count = points.len() as f64;
    let mean_x = xs.iter().sum::<f64>() / count;
    let mean_y = ys.iter().sum::<f64>() / count;
    let sxx = xs.iter().map(|x| (x - mean_x).powi(2)).sum::<f64>();
    if sxx <= f64::EPSILON {
        return empty("insufficient_data", "Samples do not span distinct timestamps.".into());
    }

    let slope = xs
        .iter()
        .zip(&ys)
        .map(|(x, y)| (x - mean_x) * (y - mean_y))
        .sum::<f64>()
        / sxx;
    let intercept = mean_y - slope * mean_x;
    let residual_sum_squares = xs
        .iter()
        .zip(&ys)
        .map(|(x, y)| (y - (intercept + slope * x)).powi(2))
        .sum::<f64>();
    let rmse = (residual_sum_squares / count).sqrt();
    let total_sum_squares = ys.iter().map(|y| (y - mean_y).powi(2)).sum::<f64>();
    let r_squared = if total_sum_squares <= f64::EPSILON {
        if residual_sum_squares <= f64::EPSILON { 1.0 } else { 0.0 }
    } else {
        (1.0 - residual_sum_squares / total_sum_squares).clamp(0.0, 1.0)
    };
    let latest_x = *xs.last().expect("at least three samples");
    let current_estimate = intercept + slope * latest_x;
    let base = MetricForecast {
        name: name.to_owned(),
        unit: unit.to_owned(),
        threshold: Some(threshold),
        sample_count: points.len(),
        first_sample_at,
        last_sample_at,
        current_estimate: Some(*ys.last().expect("at least three samples")),
        trend_per_day: Some(slope),
        r_squared: Some(r_squared),
        projected_days: None,
        interval_95_days: None,
        status: String::new(),
        detail: None,
    };

    if current_estimate >= threshold {
        return MetricForecast {
            status: "at_or_over_capacity".into(),
            detail: Some("Latest fitted level is at or above the configured threshold.".into()),
            ..base
        };
    }
    if slope <= 0.0 {
        return MetricForecast {
            status: "no_growth".into(),
            detail: Some("Fitted trend is flat or declining; no exhaustion date projected.".into()),
            ..base
        };
    }

    let projected_days = (threshold - current_estimate) / slope;
    let margin = RESIDUAL_BAND_Z * rmse;
    let earliest = ((threshold - margin - current_estimate) / slope).max(0.0);
    let latest = ((threshold + margin - current_estimate) / slope).max(0.0);
    MetricForecast {
        projected_days: Some(projected_days),
        interval_95_days: Some((earliest.min(latest), earliest.max(latest))),
        status: "projected".into(),
        detail: Some(format!(
            "Approximate 95% residual band uses +/- 1.96 x RMSE ({margin:.4} {unit}); \
             it does not account for structural changes or seasonality."
        )),
        ..base
    }
}

pub fn parse_prometheus_matrix(body: &str) -> Result<Vec<DataPoint>> {
    let response: PrometheusResponse =
        serde_json::from_str(body).context("parsing Prometheus range response")?;
    if response.status != "success" {
        bail!("Prometheus query failed: {}", response.error.unwrap_or_default());
    }
    Ok(response
        .data
        .unwrap_or(PrometheusData { result: Vec::new() })
        .result
        .into_iter()
        .flat_map(|series| series.values)
        .filter_map(|(timestamp, value)| {
            let value = value.parse::<f64>().ok()?;
            (timestamp.is_finite() && value.is_finite())
                .then_some(DataPoint { timestamp: timestamp as i64, value })
        })
        .collect())
}

fn query_prometheus(
    url: &str,
    token: Option<&str>,
    query: &str,
    start: i64,
    end: i64,
    step_secs: i64,
) -> Result<Vec<DataPoint>> {
    let mut command = ProcessCommand::new("curl");
    command
        .args(["-fsS", "--get", "--max-time", "60"])
        .arg("--data-urlencode")
        .arg(format!("query={query}"))
        .arg("--data-urlencode")
        .arg(format!("start={start}"))
        .arg("--data-urlencode")
        .arg(format!("end={end}"))
        .arg("--data-urlencode")
        .arg(format!("step={step_secs}"));
    if let Some(token) = token {
        command.arg("-H").arg(format!("Authorization: Bearer {token}"));
    }
    let output = command
        .arg(format!("{}/api/v1/query_range", url.trim_end_matches('/')))
        .output()
        .context("running curl for Prometheus query")?;
    if !output.status.success() {
        bail!("Prometheus query failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    parse_prometheus_matrix(&String::from_utf8_lossy(&output.stdout))
}

fn markdown_report(report: &ForecastReport) -> String {
    let mut output = format!(
        "# Capacity Forecast\n\nGenerated: {}\n\nHistorical window: {} days\n\nUncertainty: {}\n\n",
        report.generated_at, report.window_days, report.uncertainty_method
    );
    output.push_str(
        "| Dimension | Current | Capacity | Trend / day | Time to capacity | \
         Approx. 95% interval | Status |\n",
    );
    output.push_str("|---|---:|---:|---:|---:|---:|---|\n");
    for forecast in &report.forecasts {
        let value = |number: Option<f64>| {
            number.map_or_else(|| "n/a".to_owned(), |number| format!("{number:.3}"))
        };
        let interval = forecast.interval_95_days.map_or_else(
            || "n/a".to_owned(),
            |(low, high)| format!("{low:.1} to {high:.1} days"),
        );
        let time = forecast.projected_days.map_or_else(
            || "n/a".to_owned(),
            |days| format!("{days:.1} days ({:.1} weeks)", days / 7.0),
        );
        output.push_str(&format!(
            "| {} | {} {} | {} {} | {} {} | {} | {} | {} |\n",
            forecast.name,
            value(forecast.current_estimate),
            forecast.unit,
            value(forecast.threshold),
            forecast.unit,
            value(forecast.trend_per_day),
            forecast.unit,
            time,
            interval,
            forecast.status
        ));
        output.push_str(&format!(
            "\n{} model: {} samples, R-squared {}.\n",
            forecast.name,
            forecast.sample_count,
            forecast
                .r_squared
                .map_or_else(|| "n/a".to_owned(), |value| format!("{value:.3}"))
        ));
        if let Some(detail) = &forecast.detail {
            output.push_str(&format!("\n{}: {}\n", forecast.name, detail));
        }
    }
    output.push_str(
        "\nForecasts are reporting estimates only. Linear trends assume historical growth \
         continues; validate against known seasonality, planned changes, and load-test \
         results before making capacity decisions.\n",
    );
    output
}

pub fn run(args: CapacityForecastArgs) -> Result<()> {
    if args.window_days < 1 || args.step_secs < 1 {
        bail!("window-days and step-secs must be positive");
    }
    if !(0.0..=1.0).contains(&args.pool_threshold) {
        bail!("pool-threshold must be between 0 and 1");
    }
    if !args.throughput_capacity_rps.is_finite() || args.throughput_capacity_rps <= 0.0 {
        bail!("throughput-capacity-rps must be a positive finite number");
    }
    let prometheus_url = args
        .prometheus_url
        .as_deref()
        .filter(|url| !url.trim().is_empty())
        .context("set PROMETHEUS_URL or pass --prometheus-url")?;
    let storage_capacity_bytes = parse_storage_capacity(args.storage_capacity_bytes.as_deref())?;
    let prometheus_token = args
        .prometheus_token
        .as_deref()
        .filter(|token| !token.is_empty());

    let end = Utc::now();
    let start = end - Duration::days(args.window_days);
    let start_ts = start.timestamp();
    let end_ts = end.timestamp();
    let mut forecasts = Vec::new();
    let queries = [
        (
            "Database pool saturation",
            "ratio",
            args.pool_query.as_str(),
            Some(args.pool_threshold),
        ),
        (
            "Database storage",
            "bytes",
            args.storage_query.as_str(),
            storage_capacity_bytes,
        ),
        (
            "Request throughput",
            "requests/s",
            args.throughput_query.as_str(),
            Some(args.throughput_capacity_rps),
        ),
    ];

    for (name, unit, query, threshold) in queries {
        let result = query_prometheus(
            prometheus_url,
            prometheus_token,
            query,
            start_ts,
            end_ts,
            args.step_secs,
        );
        let forecast = match result {
            Ok(points) => forecast(name, unit, &points, threshold),
            Err(error) => {
                let mut forecast = forecast(name, unit, &[], threshold);
                forecast.status = "query_error".into();
                forecast.detail = Some(error.to_string());
                forecast
            }
        };
        forecasts.push(forecast);
    }

    let report = ForecastReport {
        generated_at: end.to_rfc3339(),
        window_days: args.window_days,
        uncertainty_method: format!(
            "OLS linear regression; threshold crossing interval from +/- {:.2} x residual RMSE",
            RESIDUAL_BAND_Z
        ),
        forecasts,
    };
    fs::create_dir_all(&args.out_dir)
        .with_context(|| format!("creating report directory {}", args.out_dir.display()))?;
    let markdown_path = args.out_dir.join("capacity-forecast.md");
    let json_path = args.out_dir.join("capacity-forecast.json");
    fs::write(&markdown_path, markdown_report(&report))
        .with_context(|| format!("writing {}", markdown_path.display()))?;
    fs::write(&json_path, serde_json::to_vec_pretty(&report)?)
        .with_context(|| format!("writing {}", json_path.display()))?;
    println!(
        "Capacity forecast written to {} and {}",
        markdown_path.display(),
        json_path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daily_points(values: &[f64]) -> Vec<DataPoint> {
        values
            .iter()
            .enumerate()
            .map(|(day, value)| DataPoint {
                timestamp: day as i64 * 86_400,
                value: *value,
            })
            .collect()
    }

    #[test]
    fn forecasts_known_linear_growth_and_zero_residual_interval() {
        let result = forecast(
            "test",
            "units",
            &daily_points(&[10.0, 12.0, 14.0, 16.0]),
            Some(30.0),
        );

        assert_eq!(result.status, "projected");
        assert!((result.trend_per_day.unwrap() - 2.0).abs() < 1e-9);
        assert!((result.projected_days.unwrap() - 7.0).abs() < 1e-9);
        let (low, high) = result.interval_95_days.unwrap();
        assert!((low - 7.0).abs() < 1e-9);
        assert!((high - 7.0).abs() < 1e-9);
    }

    #[test]
    fn uncertainty_band_contains_noisy_growth_projection() {
        let result = forecast(
            "test",
            "units",
            &daily_points(&[10.0, 12.5, 13.5, 16.0]),
            Some(30.0),
        );
        let (earliest, latest) = result.interval_95_days.unwrap();

        assert!(earliest < result.projected_days.unwrap());
        assert!(latest > result.projected_days.unwrap());
        assert!(result.r_squared.unwrap() > 0.9);
    }

    #[test]
    fn reports_flat_trend_without_fabricating_an_exhaustion_date() {
        let result = forecast("test", "units", &daily_points(&[4.0, 4.0, 4.0]), Some(10.0));

        assert_eq!(result.status, "no_growth");
        assert!(result.projected_days.is_none());
    }

    #[test]
    fn reports_unconfigured_capacity_and_insufficient_samples() {
        let missing_capacity = forecast("storage", "bytes", &daily_points(&[1.0, 2.0, 3.0]), None);
        let insufficient = forecast("pool", "ratio", &daily_points(&[0.1, 0.2]), Some(0.8));

        assert_eq!(missing_capacity.status, "unconfigured");
        assert_eq!(insufficient.status, "insufficient_data");
    }

    #[test]
    fn blank_storage_capacity_is_unconfigured_and_invalid_values_are_rejected() {
        assert_eq!(parse_storage_capacity(Some(" ")).unwrap(), None);
        assert_eq!(parse_storage_capacity(Some("1024")).unwrap(), Some(1024.0));
        assert!(parse_storage_capacity(Some("NaN")).is_err());
        assert!(parse_storage_capacity(Some("-1")).is_err());
    }

    #[test]
    fn parses_prometheus_samples_and_ignores_non_finite_values() {
        let body = r#"{"status":"success","data":{"result":[{"values":[[1,"2.5"],[2,"NaN"]]}]}}"#;
        let points = parse_prometheus_matrix(body).unwrap();

        assert_eq!(points, vec![DataPoint { timestamp: 1, value: 2.5 }]);
    }
}