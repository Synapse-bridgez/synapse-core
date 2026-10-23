//! HMAC-SHA256 signing for outbound swap webhooks (#1520).
//!
//! Signed string (exact bytes, no re-serialization):
//!   `"{timestamp}.{delivery_id}.{raw_body}"`
//! Header value: `t=<unix seconds>,id=<delivery id>,v1=<hex hmac>[,v1=<hex hmac>]`
//! Multiple `v1` values are emitted during a rotation grace period (one per
//! active secret) so receivers holding either secret can verify.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub const SIGNATURE_HEADER: &str = "X-Synapse-Signature";
/// Default replay window (seconds). Also bounds tolerated clock skew.
pub const DEFAULT_TOLERANCE_SECS: i64 = 300;

#[derive(Debug, PartialEq, Eq)]
pub enum VerifyError {
    Malformed,
    OutsideTolerance,
    BadSignature,
}

fn mac(secret: &[u8], timestamp: i64, delivery_id: &str, body: &[u8]) -> HmacSha256 {
    let mut m = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    m.update(timestamp.to_string().as_bytes());
    m.update(b".");
    m.update(delivery_id.as_bytes());
    m.update(b".");
    m.update(body);
    m
}

pub fn sign(secret: &[u8], timestamp: i64, delivery_id: &str, body: &[u8]) -> String {
    hex::encode(
        mac(secret, timestamp, delivery_id, body)
            .finalize()
            .into_bytes(),
    )
}

/// Build the header value, signing with every active secret (current first,
/// previous during the rotation grace period).
pub fn header_value(secrets: &[&[u8]], timestamp: i64, delivery_id: &str, body: &[u8]) -> String {
    let mut out = format!("t={timestamp},id={delivery_id}");
    for s in secrets {
        out.push_str(",v1=");
        out.push_str(&sign(s, timestamp, delivery_id, body));
    }
    out
}

/// Reference verifier: accepts if any `v1` matches any provided secret
/// (constant-time compare) and the timestamp is within `tolerance_secs`.
pub fn verify(
    secrets: &[&[u8]],
    header: &str,
    body: &[u8],
    now: i64,
    tolerance_secs: i64,
) -> Result<(), VerifyError> {
    let (mut ts, mut id, mut sigs) = (None, None, Vec::new());
    for part in header.split(',') {
        match part.split_once('=') {
            Some(("t", v)) => ts = v.parse::<i64>().ok(),
            Some(("id", v)) => id = Some(v),
            Some(("v1", v)) => sigs.push(v),
            _ => {}
        }
    }
    let (ts, id) = match (ts, id) {
        (Some(t), Some(i)) if !sigs.is_empty() => (t, i),
        _ => return Err(VerifyError::Malformed),
    };
    if (now - ts).abs() > tolerance_secs {
        return Err(VerifyError::OutsideTolerance);
    }
    for secret in secrets {
        for sig in &sigs {
            if let Ok(bytes) = hex::decode(sig) {
                if mac(secret, ts, id, body).verify_slice(&bytes).is_ok() {
                    return Ok(());
                }
            }
        }
    }
    Err(VerifyError::BadSignature)
}

/// Per-tenant secret pair supporting rotation with a grace period.
#[derive(Clone)]
pub struct SigningSecrets {
    current: Vec<u8>,
    previous: Option<(Vec<u8>, i64)>, // (secret, valid until unix secs)
}

impl SigningSecrets {
    pub fn generate() -> (Self, String) {
        let s = Self::new_secret();
        (
            Self {
                current: s.clone().into_bytes(),
                previous: None,
            },
            s,
        )
    }

    fn new_secret() -> String {
        use rand::RngCore;
        let mut b = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut b);
        format!("whsec_{}", hex::encode(b))
    }

    /// Rotate; the old secret keeps signing/verifying until `now + grace_secs`.
    /// Returns the new secret (shown once, never echoed again).
    pub fn rotate(&mut self, now: i64, grace_secs: i64) -> String {
        let new = Self::new_secret();
        let old = std::mem::replace(&mut self.current, new.clone().into_bytes());
        self.previous = Some((old, now + grace_secs));
        new
    }

    /// Secrets active at `now` (current first).
    pub fn active(&self, now: i64) -> Vec<&[u8]> {
        let mut v = vec![self.current.as_slice()];
        if let Some((p, until)) = &self.previous {
            if now <= *until {
                v.push(p.as_slice());
            }
        }
        v
    }
}

impl std::fmt::Debug for SigningSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SigningSecrets(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &[u8] = br#"{"event":"swap.settled","id":"s1"}"#;

    #[test]
    fn known_answer() {
        // Cross-checked with Python: hmac.new(b"secret", b"1700000000.d1." + body, sha256)
        assert_eq!(
            sign(b"secret", 1_700_000_000, "d1", BODY),
            "859fdc894b5fdfefcb92e55f810e68ae16f3203a1414195805afc80661c9380a"
        );
    }

    #[test]
    fn roundtrip_and_tamper() {
        let h = header_value(&[b"secret"], 1000, "d1", BODY);
        assert_eq!(verify(&[b"secret"], &h, BODY, 1000, 300), Ok(()));
        assert_eq!(
            verify(&[b"secret"], &h, b"{}", 1000, 300),
            Err(VerifyError::BadSignature)
        );
        assert_eq!(
            verify(&[b"other"], &h, BODY, 1000, 300),
            Err(VerifyError::BadSignature)
        );
    }

    #[test]
    fn replay_outside_window_rejected() {
        let h = header_value(&[b"secret"], 1000, "d1", BODY);
        assert_eq!(verify(&[b"secret"], &h, BODY, 1300, 300), Ok(()));
        assert_eq!(
            verify(&[b"secret"], &h, BODY, 1301, 300),
            Err(VerifyError::OutsideTolerance)
        );
        assert_eq!(
            verify(&[b"secret"], &h, BODY, 699, 300),
            Err(VerifyError::OutsideTolerance)
        );
    }

    #[test]
    fn rotation_grace_period() {
        let (mut s, old) = SigningSecrets::generate();
        let new = s.rotate(1000, 600);
        // In-flight retry signed with the old secret still verifies in grace.
        let h_old = header_value(&[old.as_bytes()], 1000, "d1", BODY);
        assert_eq!(verify(&s.active(1100), &h_old, BODY, 1100, 300), Ok(()));
        // Dual-signed delivery verifies with either secret.
        let h = header_value(&s.active(1100), 1100, "d2", BODY);
        assert_eq!(verify(&[old.as_bytes()], &h, BODY, 1100, 300), Ok(()));
        assert_eq!(verify(&[new.as_bytes()], &h, BODY, 1100, 300), Ok(()));
        // After grace the old secret is dropped.
        assert_eq!(s.active(1601).len(), 1);
        assert_eq!(
            verify(&s.active(1601), &h_old, BODY, 1100, 300),
            Err(VerifyError::BadSignature)
        );
    }

    #[test]
    fn malformed_header() {
        assert_eq!(
            verify(&[b"s"], "garbage", BODY, 0, 300),
            Err(VerifyError::Malformed)
        );
    }

    #[test]
    fn secrets_never_in_debug_output() {
        let (s, plain) = SigningSecrets::generate();
        assert!(!format!("{s:?}").contains(&plain));
    }
}
