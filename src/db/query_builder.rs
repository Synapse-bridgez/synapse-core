//! Dynamic query builder for internal/admin transaction queries.
//!
//! # ⚠ Security Notice — String Interpolation
//!
//! Unlike the parameterised queries in `src/db/queries.rs`, this builder
//! constructs SQL by **string interpolation**.  This is intentional for the
//! admin/reporting use-case where the full set of filter combinations cannot be
//! expressed with a fixed number of `$N` placeholders, but it means that
//! **callers are responsible for ensuring all inputs are validated before
//! calling any builder method**.
//!
//! ## Safe usage contract
//!
//! Every value passed to a builder method must satisfy one of:
//!
//! - **Typed** — `BigDecimal`, `DateTime<Utc>`, `Uuid` values are formatted by
//!   their `Display` implementations which produce safe, well-defined output.
//! - **Validated string** — string values (`status`, `asset_code`,
//!   `stellar_account`) must be validated against an allow-list or regex
//!   **before** being passed here.  The validation layer in
//!   `src/validation/mod.rs` enforces this for all HTTP-originated inputs.
//!
//! ## What this builder must NOT be used for
//!
//! - Raw user-supplied strings that have not been validated.
//! - Any query that runs in a user-facing request path without prior validation.
//!
//! For user-facing queries, use the parameterised functions in
//! `src/db/queries.rs` instead.
//!
//! ## Planned improvement
//!
//! This builder should be migrated to sqlx's `QueryBuilder` API
//! (`sqlx::QueryBuilder::push_bind`) which supports dynamic parameterised
//! queries.  Until that migration is complete, all callers must validate inputs
//! at the boundary.  See `docs/database-input-validation.md` for details.

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
