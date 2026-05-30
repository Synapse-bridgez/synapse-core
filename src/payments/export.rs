/// Data export for the Payments module (settlement logic).
use serde::Serialize;

/// Supported export formats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
}

impl ExportFormat {
    /// Parse from a string slice; defaults to CSV on unknown input.
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "json" => ExportFormat::Json,
            _ => ExportFormat::Csv,
        }
    }

    pub fn content_type(&self) -> &'static str {
        match self {
            ExportFormat::Csv => "text/csv",
            ExportFormat::Json => "application/json",
        }
    }
}

/// A single row in a settlement export.
#[derive(Debug, Serialize)]
pub struct SettlementExportRow {
    pub id: String,
    pub asset_code: String,
    pub total_amount: String,
    pub tx_count: i32,
    pub status: String,
    pub period_start: String,
    pub period_end: String,
}

/// Serialize rows to CSV string.
pub fn to_csv(rows: &[SettlementExportRow]) -> Result<String, String> {
    let mut wtr = csv::Writer::from_writer(vec![]);
    for row in rows {
        wtr.serialize(row).map_err(|e| e.to_string())?;
    }
    String::from_utf8(wtr.into_inner().map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// Serialize rows to JSON string.
pub fn to_json(rows: &[SettlementExportRow]) -> Result<String, String> {
    serde_json::to_string(rows).map_err(|e| e.to_string())
}

/// Export settlements in the requested format.
pub fn export_settlements(
    rows: &[SettlementExportRow],
    format: ExportFormat,
) -> Result<String, String> {
    match format {
        ExportFormat::Csv => to_csv(rows),
        ExportFormat::Json => to_json(rows),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_row() -> SettlementExportRow {
        SettlementExportRow {
            id: "uuid-1".into(),
            asset_code: "USD".into(),
            total_amount: "1000.00".into(),
            tx_count: 5,
            status: "completed".into(),
            period_start: "2025-01-01T00:00:00Z".into(),
            period_end: "2025-01-31T23:59:59Z".into(),
        }
    }

    #[test]
    fn test_export_format_from_str() {
        assert_eq!(ExportFormat::from_str("json"), ExportFormat::Json);
        assert_eq!(ExportFormat::from_str("JSON"), ExportFormat::Json);
        assert_eq!(ExportFormat::from_str("csv"), ExportFormat::Csv);
        assert_eq!(ExportFormat::from_str("unknown"), ExportFormat::Csv);
    }

    #[test]
    fn test_content_type() {
        assert_eq!(ExportFormat::Csv.content_type(), "text/csv");
        assert_eq!(ExportFormat::Json.content_type(), "application/json");
    }

    #[test]
    fn test_to_csv_contains_header_and_data() {
        let rows = vec![sample_row()];
        let result = to_csv(&rows).unwrap();
        assert!(result.contains("asset_code"));
        assert!(result.contains("USD"));
    }

    #[test]
    fn test_to_json_valid() {
        let rows = vec![sample_row()];
        let result = to_json(&rows).unwrap();
        assert!(result.contains("\"asset_code\":\"USD\""));
    }

    #[test]
    fn test_export_settlements_csv() {
        let rows = vec![sample_row()];
        let out = export_settlements(&rows, ExportFormat::Csv).unwrap();
        assert!(out.contains("USD"));
    }

    #[test]
    fn test_export_settlements_json() {
        let rows = vec![sample_row()];
        let out = export_settlements(&rows, ExportFormat::Json).unwrap();
        assert!(out.contains("completed"));
    }

    #[test]
    fn test_export_empty_rows() {
        let out_csv = export_settlements(&[], ExportFormat::Csv).unwrap();
        // CSV with no data rows still has a header
        assert!(out_csv.contains("id"));

        let out_json = export_settlements(&[], ExportFormat::Json).unwrap();
        assert_eq!(out_json, "[]");
    }
}
