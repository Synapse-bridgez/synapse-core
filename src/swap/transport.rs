//! Shared outbound transport policy for liquidity provider adapters.
//!
//! Adapters obtain HTTP clients only through [`build_client`], which enforces
//! mTLS (client identity + pinned trust store), hostname verification, timeouts,
//! and fails closed on missing/expired/unparseable material. [`RequestSigner`]
//! produces HMAC-SHA256 request signatures bound to a timestamp.

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use reqwest::{Certificate, Client, Identity};
use sha2::Sha256;
use std::time::Duration;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransportError {
    #[error("client certificate or key missing")]
    MissingClientIdentity,
    #[error("trust store missing")]
    MissingTrustStore,
    #[error("client certificate expired at {0}")]
    CertificateExpired(DateTime<Utc>),
    #[error("invalid TLS material: {0}")]
    InvalidMaterial(String),
}

/// Centralized TLS configuration for LP connections. PEM contents are held in
/// memory (loaded from secret mounts by the caller); `Debug` redacts them.
#[derive(Clone)]
pub struct TlsConfig {
    pub client_cert_pem: Vec<u8>,
    pub client_key_pem: Vec<u8>,
    pub ca_pem: Vec<u8>,
    /// Expiry of the client certificate, supplied by the secret provider.
    pub client_cert_not_after: DateTime<Utc>,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
}

impl std::fmt::Debug for TlsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsConfig")
            .field("client_cert_not_after", &self.client_cert_not_after)
            .field("connect_timeout", &self.connect_timeout)
            .field("request_timeout", &self.request_timeout)
            .finish_non_exhaustive()
    }
}

impl TlsConfig {
    pub fn validate(&self, now: DateTime<Utc>) -> Result<(), TransportError> {
        if self.client_cert_pem.is_empty() || self.client_key_pem.is_empty() {
            return Err(TransportError::MissingClientIdentity);
        }
        if self.ca_pem.is_empty() {
            return Err(TransportError::MissingTrustStore);
        }
        if self.client_cert_not_after <= now {
            return Err(TransportError::CertificateExpired(
                self.client_cert_not_after,
            ));
        }
        Ok(())
    }
}

/// Build the shared HTTP client. Rebuild it on certificate rotation (the old
/// client keeps serving in-flight requests). Failures bump the security metric
/// via the returned error; callers must not fall back to plain TLS.
pub fn build_client(cfg: &TlsConfig) -> Result<Client, TransportError> {
    cfg.validate(Utc::now())?;
    let identity = Identity::from_pkcs8_pem(&cfg.client_cert_pem, &cfg.client_key_pem)
        .map_err(|e| TransportError::InvalidMaterial(e.to_string()))?;
    let ca = Certificate::from_pem(&cfg.ca_pem)
        .map_err(|e| TransportError::InvalidMaterial(e.to_string()))?;
    Client::builder()
        .identity(identity)
        .tls_built_in_root_certs(false)
        .add_root_certificate(ca)
        .danger_accept_invalid_certs(false)
        .danger_accept_invalid_hostnames(false)
        .https_only(true)
        .connect_timeout(cfg.connect_timeout)
        .timeout(cfg.request_timeout)
        .build()
        .map_err(|e| TransportError::InvalidMaterial(e.to_string()))
}

/// HMAC-SHA256 request signer. Signature covers `timestamp.method.path.body`.
pub struct RequestSigner {
    key: Vec<u8>,
}

impl RequestSigner {
    pub fn new(key: impl Into<Vec<u8>>) -> Self {
        Self { key: key.into() }
    }

    pub fn sign(&self, timestamp: i64, method: &str, path: &str, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("HMAC accepts any key length");
        mac.update(timestamp.to_string().as_bytes());
        mac.update(b".");
        mac.update(method.to_ascii_uppercase().as_bytes());
        mac.update(b".");
        mac.update(path.as_bytes());
        mac.update(b".");
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    /// Constant-time verification, rejecting timestamps outside `tolerance`.
    pub fn verify(
        &self,
        timestamp: i64,
        method: &str,
        path: &str,
        body: &[u8],
        signature: &str,
        now: i64,
        tolerance_secs: i64,
    ) -> bool {
        if (now - timestamp).abs() > tolerance_secs {
            return false;
        }
        let Ok(sig) = hex::decode(signature) else {
            return false;
        };
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("HMAC accepts any key length");
        mac.update(timestamp.to_string().as_bytes());
        mac.update(b".");
        mac.update(method.to_ascii_uppercase().as_bytes());
        mac.update(b".");
        mac.update(path.as_bytes());
        mac.update(b".");
        mac.update(body);
        mac.verify_slice(&sig).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(not_after: DateTime<Utc>) -> TlsConfig {
        TlsConfig {
            client_cert_pem: b"c".to_vec(),
            client_key_pem: b"k".to_vec(),
            ca_pem: b"ca".to_vec(),
            client_cert_not_after: not_after,
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(30),
        }
    }

    #[test]
    fn fails_closed_on_expired_cert() {
        let c = cfg(Utc::now() - chrono::Duration::seconds(1));
        assert!(matches!(
            build_client(&c),
            Err(TransportError::CertificateExpired(_))
        ));
    }

    #[test]
    fn fails_closed_on_missing_material() {
        let mut c = cfg(Utc::now() + chrono::Duration::days(1));
        c.client_key_pem.clear();
        assert_eq!(
            c.validate(Utc::now()),
            Err(TransportError::MissingClientIdentity)
        );
        let mut c = cfg(Utc::now() + chrono::Duration::days(1));
        c.ca_pem.clear();
        assert_eq!(
            c.validate(Utc::now()),
            Err(TransportError::MissingTrustStore)
        );
    }

    #[test]
    fn untrusted_material_is_rejected() {
        let c = cfg(Utc::now() + chrono::Duration::days(1));
        assert!(matches!(
            build_client(&c),
            Err(TransportError::InvalidMaterial(_))
        ));
    }

    #[test]
    fn debug_redacts_pem() {
        assert!(!format!("{:?}", cfg(Utc::now())).contains("client_key_pem"));
    }

    #[test]
    fn signing_roundtrip_and_tamper() {
        let s = RequestSigner::new(b"secret".to_vec());
        let sig = s.sign(100, "post", "/orders", b"{}");
        assert!(s.verify(100, "POST", "/orders", b"{}", &sig, 110, 30));
        assert!(!s.verify(100, "POST", "/orders", b"{\"a\":1}", &sig, 110, 30));
        assert!(!s.verify(100, "POST", "/orders", b"{}", &sig, 500, 30));
    }
}
