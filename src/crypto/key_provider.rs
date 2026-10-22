use super::CryptoError;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use rand::RngCore;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, RwLock};

const NONCE_LEN: usize = 12;

/// Wraps and unwraps per-record data-encryption keys (DEKs) with a master key
/// (KEK) that lives in a keystore. Implementations must never log key bytes.
pub trait KeyProvider: Send + Sync {
    /// Key version used for new writes.
    fn current_version(&self) -> u32;
    fn wrap(&self, version: u32, dek: &[u8]) -> Result<Vec<u8>, CryptoError>;
    fn unwrap(&self, version: u32, wrapped: &[u8]) -> Result<Vec<u8>, CryptoError>;
    /// Create a new key version and make it current; returns the new version.
    fn rotate(&self) -> Result<u32, CryptoError>;
}

fn aead_seal(kek: &[u8; 32], data: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let cipher = Aes256Gcm::new_from_slice(kek).map_err(|_| CryptoError::EncryptFailed)?;
    let mut nonce = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce);
    let mut out = nonce.to_vec();
    out.extend(
        cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: data,
                    aad: b"dek",
                },
            )
            .map_err(|_| CryptoError::EncryptFailed)?,
    );
    Ok(out)
}

fn aead_open(kek: &[u8; 32], sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if sealed.len() <= NONCE_LEN {
        return Err(CryptoError::DecryptFailed);
    }
    let (nonce, ct) = sealed.split_at(NONCE_LEN);
    Aes256Gcm::new_from_slice(kek)
        .map_err(|_| CryptoError::DecryptFailed)?
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ct,
                aad: b"dek",
            },
        )
        .map_err(|_| CryptoError::DecryptFailed)
}

/// Local-development provider: KEKs are held in process memory (seeded from a
/// 32-byte secret). Not for production use.
#[derive(Clone)]
pub struct LocalKeyProvider {
    keys: Arc<RwLock<BTreeMap<u32, [u8; 32]>>>,
}

impl LocalKeyProvider {
    pub fn new(initial_kek: [u8; 32]) -> Self {
        Self {
            keys: Arc::new(RwLock::new(BTreeMap::from([(1, initial_kek)]))),
        }
    }

    pub fn from_hex(hex_kek: &str) -> Result<Self, CryptoError> {
        let bytes =
            hex::decode(hex_kek).map_err(|_| CryptoError::Provider("invalid KEK hex".into()))?;
        let kek: [u8; 32] = bytes
            .try_into()
            .map_err(|_| CryptoError::Provider("KEK must be 32 bytes".into()))?;
        Ok(Self::new(kek))
    }

    fn kek(&self, version: u32) -> Result<[u8; 32], CryptoError> {
        self.keys
            .read()
            .map_err(|_| CryptoError::Provider("lock poisoned".into()))?
            .get(&version)
            .copied()
            .ok_or(CryptoError::UnknownKeyVersion(version))
    }
}

impl fmt::Debug for LocalKeyProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LocalKeyProvider(<redacted>)")
    }
}

impl KeyProvider for LocalKeyProvider {
    fn current_version(&self) -> u32 {
        self.keys
            .read()
            .ok()
            .and_then(|k| k.keys().next_back().copied())
            .unwrap_or(1)
    }

    fn wrap(&self, version: u32, dek: &[u8]) -> Result<Vec<u8>, CryptoError> {
        aead_seal(&self.kek(version)?, dek)
    }

    fn unwrap(&self, version: u32, wrapped: &[u8]) -> Result<Vec<u8>, CryptoError> {
        aead_open(&self.kek(version)?, wrapped)
    }

    fn rotate(&self) -> Result<u32, CryptoError> {
        let mut keys = self
            .keys
            .write()
            .map_err(|_| CryptoError::Provider("lock poisoned".into()))?;
        let next = keys.keys().next_back().copied().unwrap_or(0) + 1;
        let mut kek = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut kek);
        keys.insert(next, kek);
        Ok(next)
    }
}

/// Minimal client surface of an external KMS/HSM. The master key never leaves
/// the keystore; only wrap/unwrap requests cross the boundary.
pub trait KmsClient: Send + Sync {
    fn current_key_version(&self) -> Result<u32, CryptoError>;
    fn wrap_key(&self, version: u32, dek: &[u8]) -> Result<Vec<u8>, CryptoError>;
    fn unwrap_key(&self, version: u32, wrapped: &[u8]) -> Result<Vec<u8>, CryptoError>;
    fn create_key_version(&self) -> Result<u32, CryptoError>;
}

/// External KMS/HSM-backed provider. Vendor SDKs plug in via [`KmsClient`].
pub struct KmsKeyProvider<C: KmsClient> {
    client: C,
}

impl<C: KmsClient> KmsKeyProvider<C> {
    pub fn new(client: C) -> Self {
        Self { client }
    }
}

impl<C: KmsClient> KeyProvider for KmsKeyProvider<C> {
    fn current_version(&self) -> u32 {
        self.client.current_key_version().unwrap_or(1)
    }
    fn wrap(&self, version: u32, dek: &[u8]) -> Result<Vec<u8>, CryptoError> {
        self.client.wrap_key(version, dek)
    }
    fn unwrap(&self, version: u32, wrapped: &[u8]) -> Result<Vec<u8>, CryptoError> {
        self.client.unwrap_key(version, wrapped)
    }
    fn rotate(&self) -> Result<u32, CryptoError> {
        self.client.create_key_version()
    }
}

/// Keystore selection, read from the environment by `src/config.rs`.
#[derive(Clone, PartialEq, Eq)]
pub enum KeystoreConfig {
    /// `KEYSTORE_PROVIDER=local` with `KEYSTORE_LOCAL_KEK` (64 hex chars).
    Local { kek_hex: String },
    /// `KEYSTORE_PROVIDER=kms` with `KEYSTORE_KMS_KEY_ID`.
    Kms { key_id: String },
}

impl fmt::Debug for KeystoreConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local { .. } => f.write_str("KeystoreConfig::Local(<redacted>)"),
            Self::Kms { key_id } => write!(f, "KeystoreConfig::Kms({key_id})"),
        }
    }
}

impl KeystoreConfig {
    pub fn from_env() -> Result<Option<Self>, CryptoError> {
        match std::env::var("KEYSTORE_PROVIDER").ok().as_deref() {
            None | Some("") | Some("none") => Ok(None),
            Some("local") => Ok(Some(Self::Local {
                kek_hex: std::env::var("KEYSTORE_LOCAL_KEK")
                    .map_err(|_| CryptoError::Provider("KEYSTORE_LOCAL_KEK not set".into()))?,
            })),
            Some("kms") => Ok(Some(Self::Kms {
                key_id: std::env::var("KEYSTORE_KMS_KEY_ID")
                    .map_err(|_| CryptoError::Provider("KEYSTORE_KMS_KEY_ID not set".into()))?,
            })),
            Some(other) => Err(CryptoError::Provider(format!(
                "unknown KEYSTORE_PROVIDER '{other}'"
            ))),
        }
    }
}
