//! Immutable swap execution receipts and tenant statements.
//! See `docs/swap-receipts.md`.
//!
//! All period boundaries are **UTC**: a statement for `[from, to)` includes
//! receipts with `from <= settled_at < to`.

use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;
use uuid::Uuid;

use super::direction::SwapDirection;

/// Maximum statement range, to bound generation cost.
pub const MAX_STATEMENT_RANGE_DAYS: i64 = 366;

/// Column order shared by every rendering (CSV/JSON/PDF format parity).
pub const FIELDS: [&str; 14] = [
    "receipt_id",
    "order_id",
    "tenant_id",
    "direction",
    "status",
    "source_asset",
    "dest_asset",
    "executed_amount",
    "fee",
    "refunded_amount",
    "routing_path",
    "price_reference",
    "settlement_tx_id",
    "settled_at",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwapReceipt {
    pub receipt_id: Uuid,
    pub order_id: Uuid,
    pub tenant_id: Uuid,
    pub direction: SwapDirection,
    /// `settled`, `partially_filled` or `compensated`.
    pub status: String,
    pub source_asset: String,
    pub dest_asset: String,
    /// Decimal strings, to keep amounts exact.
    pub executed_amount: String,
    pub fee: String,
    /// Non-zero for partially filled / compensated orders (refunds, reversals).
    pub refunded_amount: String,
    pub routing_path: String,
    pub price_reference: String,
    pub settlement_tx_id: String,
    pub settled_at: DateTime<Utc>,
    /// SHA-256 over all other fields; set by [`SwapReceipt::seal`].
    pub content_hash: String,
}

impl SwapReceipt {
    /// Values in [`FIELDS`] order (single source for all formats).
    pub fn values(&self) -> [String; 14] {
        [
            self.receipt_id.to_string(),
            self.order_id.to_string(),
            self.tenant_id.to_string(),
            self.direction.as_str().into(),
            self.status.clone(),
            self.source_asset.clone(),
            self.dest_asset.clone(),
            self.executed_amount.clone(),
            self.fee.clone(),
            self.refunded_amount.clone(),
            self.routing_path.clone(),
            self.price_reference.clone(),
            self.settlement_tx_id.clone(),
            self.settled_at.to_rfc3339(),
        ]
    }

    pub fn compute_hash(&self) -> String {
        let mut h = Sha256::new();
        for v in self.values() {
            h.update((v.len() as u64).to_be_bytes());
            h.update(v.as_bytes());
        }
        hex::encode(h.finalize())
    }

    /// Stamp the content hash; call exactly once at settlement finalization.
    pub fn seal(mut self) -> Self {
        self.content_hash = self.compute_hash();
        self
    }

    pub fn verify(&self) -> bool {
        self.content_hash == self.compute_hash()
    }
}

#[derive(Debug, Clone, Default)]
pub struct StatementFilter {
    pub asset: Option<String>,
    pub direction: Option<SwapDirection>,
    pub status: Option<String>,
}

impl StatementFilter {
    pub fn matches(&self, r: &SwapReceipt) -> bool {
        self.asset
            .as_deref()
            .map_or(true, |a| r.source_asset == a || r.dest_asset == a)
            && self.direction.map_or(true, |d| r.direction == d)
            && self.status.as_deref().map_or(true, |s| r.status == s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StatementError {
    #[error("`to` must be after `from`")]
    InvalidRange,
    #[error("range exceeds {MAX_STATEMENT_RANGE_DAYS} days")]
    RangeTooLarge,
    #[error("download link invalid or expired")]
    BadSignature,
    #[error("render failed")]
    Render,
}

pub fn validate_range(from: DateTime<Utc>, to: DateTime<Utc>) -> Result<(), StatementError> {
    if to <= from {
        return Err(StatementError::InvalidRange);
    }
    if to - from > Duration::days(MAX_STATEMENT_RANGE_DAYS) {
        return Err(StatementError::RangeTooLarge);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementFormat {
    Csv,
    Json,
    Pdf,
}

/// Stream a statement into `out` one receipt at a time (the iterator may be a
/// DB cursor, so a full period is never held in memory). CSV and JSON write
/// incrementally; PDF necessarily buffers its text lines.
pub fn render_statement<'a, W: Write>(
    format: StatementFormat,
    receipts: impl Iterator<Item = &'a SwapReceipt>,
    filter: &StatementFilter,
    out: &mut W,
) -> Result<u64, StatementError> {
    let io = |_| StatementError::Render;
    let rows = receipts.filter(|r| filter.matches(r));
    let mut n = 0u64;
    match format {
        StatementFormat::Csv => {
            let mut w = csv::Writer::from_writer(out);
            w.write_record(FIELDS).map_err(|_| StatementError::Render)?;
            for r in rows {
                w.write_record(r.values())
                    .map_err(|_| StatementError::Render)?;
                n += 1;
            }
            w.flush().map_err(io)?;
        }
        StatementFormat::Json => {
            out.write_all(b"[").map_err(io)?;
            for r in rows {
                if n > 0 {
                    out.write_all(b",").map_err(io)?;
                }
                let obj: serde_json::Map<_, _> = FIELDS
                    .iter()
                    .zip(r.values())
                    .map(|(k, v)| (k.to_string(), serde_json::Value::String(v)))
                    .collect();
                serde_json::to_writer(&mut *out, &obj).map_err(|_| StatementError::Render)?;
                n += 1;
            }
            out.write_all(b"]").map_err(io)?;
        }
        StatementFormat::Pdf => {
            let mut lines = vec![FIELDS.join(" | ")];
            for r in rows {
                lines.push(r.values().join(" | "));
                n += 1;
            }
            out.write_all(&simple_pdf(&lines)).map_err(io)?;
        }
    }
    Ok(n)
}

/// Minimal single-font PDF with one line of text per entry (paginated).
fn simple_pdf(lines: &[String]) -> Vec<u8> {
    const PER_PAGE: usize = 50;
    let esc = |s: &str| {
        s.replace('\\', "\\\\")
            .replace('(', "\\(")
            .replace(')', "\\)")
    };
    let pages: Vec<&[String]> = lines.chunks(PER_PAGE).collect();
    let mut objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        String::new(), // pages, filled below
        "<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>".into(),
    ];
    let mut kids = Vec::new();
    for page in &pages {
        let mut content = String::from("BT /F1 6 Tf 8 TL 20 780 Td\n");
        for l in *page {
            content.push_str(&format!("({}) Tj T*\n", esc(l)));
        }
        content.push_str("ET");
        let content_id = objs.len() + 2;
        kids.push(format!("{} 0 R", objs.len() + 1));
        objs.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 842 842] /Resources << /Font << /F1 3 0 R >> >> /Contents {content_id} 0 R >>"
        ));
        objs.push(format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ));
    }
    objs[1] = format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        pages.len()
    );
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{o}\nendobj\n", i + 1));
    }
    let xref = out.len();
    out.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        objs.len() + 1
    ));
    for off in offsets {
        out.push_str(&format!("{off:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF",
        objs.len() + 1
    ));
    out.into_bytes()
}

type HmacSha256 = Hmac<Sha256>;

fn mac(secret: &[u8], tenant: Uuid, query: &str, expires: i64) -> HmacSha256 {
    let mut m = HmacSha256::new_from_slice(secret).expect("hmac accepts any key length");
    m.update(format!("{tenant}\n{query}\n{expires}").as_bytes());
    m
}

/// Signature for a time-limited statement download. `query` is the canonical
/// statement parameters (range, filters, format); binding the tenant id means
/// a link for one tenant never authorizes another tenant's statement.
pub fn sign_download(
    secret: &[u8],
    tenant: Uuid,
    query: &str,
    expires_at: DateTime<Utc>,
) -> String {
    hex::encode(
        mac(secret, tenant, query, expires_at.timestamp())
            .finalize()
            .into_bytes(),
    )
}

pub fn verify_download(
    secret: &[u8],
    tenant: Uuid,
    query: &str,
    expires_at: DateTime<Utc>,
    signature: &str,
    now: DateTime<Utc>,
) -> Result<(), StatementError> {
    let sig = hex::decode(signature).map_err(|_| StatementError::BadSignature)?;
    if now >= expires_at {
        return Err(StatementError::BadSignature);
    }
    mac(secret, tenant, query, expires_at.timestamp())
        .verify_slice(&sig)
        .map_err(|_| StatementError::BadSignature)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(status: &str) -> SwapReceipt {
        SwapReceipt {
            receipt_id: Uuid::nil(),
            order_id: Uuid::nil(),
            tenant_id: Uuid::nil(),
            direction: SwapDirection::FiatToStellar,
            status: status.into(),
            source_asset: "NGN".into(),
            dest_asset: "USDC".into(),
            executed_amount: "100.00".into(),
            fee: "0.50".into(),
            refunded_amount: "0".into(),
            routing_path: "NGN>USDC".into(),
            price_reference: "ref-1".into(),
            settlement_tx_id: "abc".into(),
            settled_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            content_hash: String::new(),
        }
        .seal()
    }

    #[test]
    fn hash_changes_when_any_field_mutates() {
        let r = receipt("settled");
        assert!(r.verify());
        let mut t = r.clone();
        t.fee = "0.51".into();
        assert!(!t.verify());
    }

    #[test]
    fn formats_carry_same_fields_and_filter_applies() {
        let rs = [receipt("settled"), receipt("compensated")];
        let f = StatementFilter {
            status: Some("settled".into()),
            ..Default::default()
        };
        let (mut csv_out, mut json_out, mut pdf_out) = (vec![], vec![], vec![]);
        assert_eq!(
            render_statement(StatementFormat::Csv, rs.iter(), &f, &mut csv_out).unwrap(),
            1
        );
        render_statement(StatementFormat::Json, rs.iter(), &f, &mut json_out).unwrap();
        render_statement(StatementFormat::Pdf, rs.iter(), &f, &mut pdf_out).unwrap();
        let csv = String::from_utf8(csv_out).unwrap();
        assert_eq!(csv.lines().next().unwrap(), FIELDS.join(","));
        let json: Vec<serde_json::Map<String, serde_json::Value>> =
            serde_json::from_slice(&json_out).unwrap();
        assert_eq!(json.len(), 1);
        assert_eq!(json[0].keys().len(), FIELDS.len());
        assert!(pdf_out.starts_with(b"%PDF-1.4") && pdf_out.ends_with(b"%%EOF"));
    }

    #[test]
    fn range_is_capped() {
        let now = Utc::now();
        assert!(validate_range(now, now + Duration::days(30)).is_ok());
        assert_eq!(
            validate_range(now, now + Duration::days(400)),
            Err(StatementError::RangeTooLarge)
        );
        assert_eq!(validate_range(now, now), Err(StatementError::InvalidRange));
    }

    #[test]
    fn signed_links_are_tenant_bound_and_expire() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let now = Utc::now();
        let exp = now + Duration::minutes(5);
        let sig = sign_download(b"k", a, "q", exp);
        assert!(verify_download(b"k", a, "q", exp, &sig, now).is_ok());
        assert!(verify_download(b"k", b, "q", exp, &sig, now).is_err());
        assert!(verify_download(b"k", a, "q2", exp, &sig, now).is_err());
        assert!(verify_download(b"k", a, "q", exp, &sig, exp).is_err());
    }
}
