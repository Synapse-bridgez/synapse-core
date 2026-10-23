//! Envelope encryption for sensitive swap data (counterparty identifiers,
//! memos, KYC/AML references). See `docs/envelope-encryption.md`.

pub mod envelope;
pub mod key_provider;
pub mod webhook_signing;

pub use envelope::{Envelope, EnvelopeCipher, RotationStatus};
pub use key_provider::{KeyProvider, KeystoreConfig, KmsClient, KmsKeyProvider, LocalKeyProvider};

/// Domain error for the crypto boundary. Never carries key material.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    #[error("unknown key version {0}")]
    UnknownKeyVersion(u32),
    #[error("decryption failed: ciphertext is corrupt or was tampered with")]
    DecryptFailed,
    #[error("encryption failed")]
    EncryptFailed,
    #[error("malformed envelope: {0}")]
    MalformedEnvelope(String),
    #[error("key provider error: {0}")]
    Provider(String),
}
