use sqlx::types::BigDecimal;
use chrono::{DateTime, Utc};
use uuid::Uuid;

pub struct TransactionQueryBuilder {
    filters: Vec<String>,
    params: Vec<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

impl TransactionQueryBuilder {
    pub fn new() -> Self {
        Self {
            filters: Vec::new(),
            params: Vec::new(),
            limit: None,
            offset: None,
        }
    }

    pub fn status(mut self, status: &str) -> Self {
        self.filters.push(format!("status = '{}'", status));
        self
    }

    pub fn asset_code(mut self, asset_code: &str) -> Self {
        self.filters.push(format!("asset_code = '{}'", asset_code));
        self
    }

    pub fn amount_range(mut self, min: &BigDecimal, max: &BigDecimal) -> Self {
        self.filters.push(format!("amount BETWEEN {} AND {}", min, max));
        self
    }

    pub fn date_range(mut self, start: DateTime<Utc>, end: DateTime<Utc>) -> Self {
        self.filters.push(format!(
            "created_at BETWEEN '{}' AND '{}'",
            start.to_rfc3339(),
            end.to_rfc3339()
        ));
        self
    }

    pub fn account(mut self, stellar_account: &str) -> Self {
        self.filters.push(format!("stellar_account = '{}'", stellar_account));
        self
    }

    pub fn cursor(mut self, limit: i64, offset: i64) -> Self {
        self.limit = Some(limit);
        self.offset = Some(offset);
        self
    }

    pub fn build_select(&self) -> String {
        let mut query = "SELECT * FROM transactions".to_string();

        if !self.filters.is_empty() {
            query.push_str(" WHERE ");
            query.push_str(&self.filters.join(" AND "));
        }

        query.push_str(" ORDER BY created_at DESC");

        if let Some(limit) = self.limit {
            query.push_str(&format!(" LIMIT {}", limit));
        }

        if let Some(offset) = self.offset {
            query.push_str(&format!(" OFFSET {}", offset));
        }

        query
    }

    pub fn build_count(&self) -> String {
        let mut query = "SELECT COUNT(*) FROM transactions".to_string();

        if !self.filters.is_empty() {
            query.push_str(" WHERE ");
            query.push_str(&self.filters.join(" AND "));
        }

        query
    }
}

impl Default for TransactionQueryBuilder {
    fn default() -> Self {
        Self::new()
    }
}
