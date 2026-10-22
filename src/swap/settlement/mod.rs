//! Stellar destination routing and memo convention for swap settlement (#1510).
//!
//! Convention (stable - embedded in on-ledger transactions):
//! * Destination: the tenant's configured account, optionally overridden per asset.
//! * Memo: `MEMO_ID` (u64) = first 8 bytes (big-endian) of
//!   `SHA-256("synapse-swap-v1" || tenant_id || order_id)`. Free text is avoided;
//!   the text variant (`swap:<base64url(order_id)>`, 27 bytes) fits the 28-byte limit.
//! * Collisions: attempt `n` hashes `n` (big-endian u32) in as well; the first
//!   attempt not taken wins. The resolved route is persisted on the order
//!   (`swap_settlement_routes`) and reused verbatim on retries/replay.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use uuid::Uuid;

pub const MEMO_TEXT_MAX_BYTES: usize = 28;
const MAX_COLLISION_ATTEMPTS: u32 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoKind {
    Id,
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memo {
    pub kind: MemoKind,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedRoute {
    pub order_id: Uuid,
    pub destination: String,
    pub memo: Memo,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TenantDestination {
    pub account: String,
    /// Per-asset-code overrides of `account`.
    #[serde(default)]
    pub asset_accounts: HashMap<String, String>,
    #[serde(default = "default_kind")]
    pub memo_kind: Option<MemoKind>,
}

fn default_kind() -> Option<MemoKind> {
    Some(MemoKind::Id)
}

/// Per-tenant destination configuration, loaded from JSON
/// (`SWAP_SETTLEMENT_DESTINATIONS`: `{"<tenant_uuid>": {"account": "G..."}}`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DestinationConfig(pub HashMap<Uuid, TenantDestination>);

impl DestinationConfig {
    pub fn from_json(s: &str) -> Result<Self, SettlementError> {
        serde_json::from_str(s).map_err(|e| SettlementError::Config(e.to_string()))
    }

    pub fn from_env() -> Result<Self, SettlementError> {
        match std::env::var("SWAP_SETTLEMENT_DESTINATIONS") {
            Ok(v) if !v.trim().is_empty() => Self::from_json(&v),
            _ => Ok(Self::default()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettlementError {
    #[error("settlement config invalid: {0}")]
    Config(String),
    #[error("no destination configured for tenant {0}")]
    NoDestination(Uuid),
    #[error("invalid destination account: {0}")]
    InvalidDestination(String),
    #[error("memo text exceeds 28 bytes")]
    MemoTooLong,
    #[error("could not find a collision-free memo")]
    MemoCollision,
    #[error("destination account does not exist")]
    AccountMissing,
    #[error("destination lacks trustline for {0}")]
    NoTrustline(String),
    #[error("destination not authorized to hold {0}")]
    NotAuthorized(String),
    #[error("destination requires a memo type that is not supported: {0}")]
    MemoRequirement(String),
}

/// Basic strkey shape check for an ed25519 public key (`G` + 55 base32 chars).
pub fn is_valid_account(account: &str) -> bool {
    account.len() == 56
        && account.starts_with('G')
        && account
            .bytes()
            .all(|b| matches!(b, b'A'..=b'Z' | b'2'..=b'7'))
}

fn digest(tenant: Uuid, order: Uuid, attempt: u32) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"synapse-swap-v1");
    h.update(tenant.as_bytes());
    h.update(order.as_bytes());
    if attempt > 0 {
        h.update(attempt.to_be_bytes());
    }
    h.finalize().into()
}

/// Pure derivation of the memo for `(tenant, order)` at a collision `attempt`.
pub fn derive_memo(
    tenant: Uuid,
    order: Uuid,
    kind: MemoKind,
    attempt: u32,
) -> Result<Memo, SettlementError> {
    let d = digest(tenant, order, attempt);
    let value = match kind {
        MemoKind::Id => u64::from_be_bytes(d[..8].try_into().unwrap()).to_string(),
        MemoKind::Text => {
            let t = format!("swap:{}", b64url(&d[..16]));
            if t.len() > MEMO_TEXT_MAX_BYTES {
                return Err(SettlementError::MemoTooLong);
            }
            t
        }
    };
    Ok(Memo { kind, value })
}

fn b64url(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        let chars = c.len() + 1;
        for i in 0..chars {
            out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// Resolve the route for an order. If `existing` is set (retry/replay) it is
/// returned unchanged so the transaction is reproduced exactly.
pub fn resolve_route(
    cfg: &DestinationConfig,
    tenant: Uuid,
    order: Uuid,
    asset_code: &str,
    existing: Option<&ResolvedRoute>,
    memo_taken: impl Fn(&str, &Memo) -> bool,
) -> Result<ResolvedRoute, SettlementError> {
    if let Some(r) = existing {
        return Ok(r.clone());
    }
    let t = cfg
        .0
        .get(&tenant)
        .ok_or(SettlementError::NoDestination(tenant))?;
    let destination = t
        .asset_accounts
        .get(asset_code)
        .unwrap_or(&t.account)
        .clone();
    if !is_valid_account(&destination) {
        return Err(SettlementError::InvalidDestination(destination));
    }
    let kind = t.memo_kind.unwrap_or(MemoKind::Id);
    for attempt in 0..MAX_COLLISION_ATTEMPTS {
        let memo = derive_memo(tenant, order, kind, attempt)?;
        if !memo_taken(&destination, &memo) {
            return Ok(ResolvedRoute {
                order_id: order,
                destination,
                memo,
            });
        }
    }
    Err(SettlementError::MemoCollision)
}

/// What the ledger says about a destination, as supplied by a Horizon adapter.
#[derive(Debug, Clone, Default)]
pub struct AccountView {
    pub exists: bool,
    pub trustlines: HashMap<String, bool>, // asset code -> authorized
    /// `config.memo_required` data entry set on the account.
    pub requires_memo: bool,
}

/// Fail-fast pre-submission validation; call again right before submit since
/// authorization can change between quote and settlement.
pub fn validate_destination(
    view: &AccountView,
    asset_code: &str,
    route: &ResolvedRoute,
) -> Result<(), SettlementError> {
    if !view.exists {
        return Err(SettlementError::AccountMissing);
    }
    match view.trustlines.get(asset_code) {
        None => return Err(SettlementError::NoTrustline(asset_code.into())),
        Some(false) => return Err(SettlementError::NotAuthorized(asset_code.into())),
        Some(true) => {}
    }
    if view.requires_memo && route.memo.value.is_empty() {
        return Err(SettlementError::MemoRequirement("empty".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACC: &str = "GAAZI4TCR3TY5OJHCTJC2A4QSY6CJWJH5IAJTGKIN2ER7LBNVKOCCWN7";

    fn cfg(t: Uuid) -> DestinationConfig {
        DestinationConfig::from_json(&format!(r#"{{"{t}":{{"account":"{ACC}"}}}}"#)).unwrap()
    }

    #[test]
    fn derivation_is_deterministic_and_tenant_scoped() {
        let (t1, t2, o) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let a = derive_memo(t1, o, MemoKind::Id, 0).unwrap();
        assert_eq!(a, derive_memo(t1, o, MemoKind::Id, 0).unwrap());
        assert_ne!(a, derive_memo(t2, o, MemoKind::Id, 0).unwrap());
        assert!(a.value.parse::<u64>().is_ok());
    }

    #[test]
    fn text_memo_respects_28_byte_limit() {
        let m = derive_memo(Uuid::new_v4(), Uuid::new_v4(), MemoKind::Text, 0).unwrap();
        assert!(m.value.len() <= MEMO_TEXT_MAX_BYTES);
        assert!(m.value.starts_with("swap:"));
    }

    #[test]
    fn collision_advances_attempt_and_retry_reuses_route() {
        let (t, o) = (Uuid::new_v4(), Uuid::new_v4());
        let first = derive_memo(t, o, MemoKind::Id, 0).unwrap();
        let r = resolve_route(&cfg(t), t, o, "USDC", None, |_, m| *m == first).unwrap();
        assert_ne!(r.memo, first);
        assert_eq!(r.memo, derive_memo(t, o, MemoKind::Id, 1).unwrap());
        let again = resolve_route(&cfg(t), t, o, "USDC", Some(&r), |_, _| true).unwrap();
        assert_eq!(again, r);
    }

    #[test]
    fn exhausted_collisions_error() {
        let t = Uuid::new_v4();
        let e = resolve_route(&cfg(t), t, Uuid::new_v4(), "USDC", None, |_, _| true);
        assert_eq!(e, Err(SettlementError::MemoCollision));
    }

    #[test]
    fn missing_tenant_and_bad_account_rejected() {
        let t = Uuid::new_v4();
        assert_eq!(
            resolve_route(&DestinationConfig::default(), t, t, "X", None, |_, _| false),
            Err(SettlementError::NoDestination(t))
        );
        let bad =
            DestinationConfig::from_json(&format!(r#"{{"{t}":{{"account":"nope"}}}}"#)).unwrap();
        assert!(matches!(
            resolve_route(&bad, t, t, "X", None, |_, _| false),
            Err(SettlementError::InvalidDestination(_))
        ));
    }

    #[test]
    fn destination_validation_is_typed() {
        let t = Uuid::new_v4();
        let r = resolve_route(&cfg(t), t, t, "USDC", None, |_, _| false).unwrap();
        let mut v = AccountView::default();
        assert_eq!(
            validate_destination(&v, "USDC", &r),
            Err(SettlementError::AccountMissing)
        );
        v.exists = true;
        assert_eq!(
            validate_destination(&v, "USDC", &r),
            Err(SettlementError::NoTrustline("USDC".into()))
        );
        v.trustlines.insert("USDC".into(), false);
        assert_eq!(
            validate_destination(&v, "USDC", &r),
            Err(SettlementError::NotAuthorized("USDC".into()))
        );
        v.trustlines.insert("USDC".into(), true);
        assert!(validate_destination(&v, "USDC", &r).is_ok());
    }
}
