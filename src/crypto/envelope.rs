use super::{key_provider::KeyProvider, CryptoError};
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rand::RngCore;
use std::collections::BTreeMap;

const NONCE_LEN: usize = 12;

/// A record payload encrypted under a per-record DEK, with the DEK wrapped by
/// the KEK at `key_version`. Stored as a single string column.
#[derive(Clone, PartialEq, Eq)]
pub struct Envelope {
    pub key_version: u32,
    pub wrapped_dek: Vec<u8>,
    pub nonce: [u8; NONCE_LEN],
    pub ciphertext: Vec<u8>,
}

impl std::fmt::Debug for Envelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Envelope(v{}, <redacted>)", self.key_version)
    }
}

impl Envelope {
    /// `v1.<key_version>.<wrapped_dek>.<nonce>.<ciphertext>` (base64 parts).
    pub fn encode(&self) -> String {
        format!(
            "v1.{}.{}.{}.{}",
            self.key_version,
            B64.encode(&self.wrapped_dek),
            B64.encode(self.nonce),
            B64.encode(&self.ciphertext)
        )
    }

    pub fn decode(s: &str) -> Result<Self, CryptoError> {
        let bad = |m: &str| CryptoError::MalformedEnvelope(m.into());
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() != 5 || parts[0] != "v1" {
            return Err(bad("expected v1.<ver>.<dek>.<nonce>.<ct>"));
        }
        let d = |p: &str| B64.decode(p).map_err(|_| bad("invalid base64"));
        Ok(Self {
            key_version: parts[1].parse().map_err(|_| bad("invalid key version"))?,
            wrapped_dek: d(parts[2])?,
            nonce: d(parts[3])?.try_into().map_err(|_| bad("invalid nonce"))?,
            ciphertext: d(parts[4])?,
        })
    }
}

/// Counts of records per key version, for the rotation status command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationStatus {
    pub current_version: u32,
    pub by_version: BTreeMap<u32, u64>,
}

impl RotationStatus {
    /// Records still wrapped under a version older than the current one.
    pub fn remaining_on_old_versions(&self) -> u64 {
        self.by_version
            .iter()
            .filter(|(v, _)| **v < self.current_version)
            .map(|(_, n)| n)
            .sum()
    }
}

/// Encrypts/decrypts at the persistence boundary so domain code only sees
/// plaintext. `aad` binds a ciphertext to its record (e.g. the row id).
pub struct EnvelopeCipher<P: KeyProvider> {
    provider: P,
}

impl<P: KeyProvider> EnvelopeCipher<P> {
    pub fn new(provider: P) -> Self {
        Self { provider }
    }

    pub fn provider(&self) -> &P {
        &self.provider
    }

    pub fn encrypt(&self, plaintext: &[u8], aad: &[u8]) -> Result<Envelope, CryptoError> {
        let mut dek = [0u8; 32];
        let mut nonce = [0u8; NONCE_LEN];
        let mut rng = rand::thread_rng();
        rng.fill_bytes(&mut dek);
        rng.fill_bytes(&mut nonce);
        let ciphertext = Aes256Gcm::new_from_slice(&dek)
            .map_err(|_| CryptoError::EncryptFailed)?
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| CryptoError::EncryptFailed)?;
        let key_version = self.provider.current_version();
        let wrapped_dek = self.provider.wrap(key_version, &dek)?;
        Ok(Envelope {
            key_version,
            wrapped_dek,
            nonce,
            ciphertext,
        })
    }

    pub fn decrypt(&self, env: &Envelope, aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let dek = self.provider.unwrap(env.key_version, &env.wrapped_dek)?;
        Aes256Gcm::new_from_slice(&dek)
            .map_err(|_| CryptoError::DecryptFailed)?
            .decrypt(
                Nonce::from_slice(&env.nonce),
                Payload {
                    msg: &env.ciphertext,
                    aad,
                },
            )
            .map_err(|_| CryptoError::DecryptFailed)
    }

    /// Re-wrap the DEK under the current key version without touching the
    /// payload. Idempotent: an envelope already on the current version is
    /// returned unchanged, so an interrupted rotation can simply resume.
    pub fn rewrap(&self, env: &Envelope) -> Result<Envelope, CryptoError> {
        let current = self.provider.current_version();
        if env.key_version == current {
            return Ok(env.clone());
        }
        let dek = self.provider.unwrap(env.key_version, &env.wrapped_dek)?;
        Ok(Envelope {
            key_version: current,
            wrapped_dek: self.provider.wrap(current, &dek)?,
            nonce: env.nonce,
            ciphertext: env.ciphertext.clone(),
        })
    }

    pub fn status<'a>(&self, envs: impl IntoIterator<Item = &'a Envelope>) -> RotationStatus {
        let mut by_version = BTreeMap::new();
        for e in envs {
            *by_version.entry(e.key_version).or_insert(0) += 1;
        }
        RotationStatus {
            current_version: self.provider.current_version(),
            by_version,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::LocalKeyProvider;

    fn cipher() -> EnvelopeCipher<LocalKeyProvider> {
        EnvelopeCipher::new(LocalKeyProvider::new([7u8; 32]))
    }

    #[test]
    fn round_trip_and_tamper_detection() {
        let c = cipher();
        let env = c.encrypt(b"GACCT123", b"row-1").unwrap();
        assert_eq!(c.decrypt(&env, b"row-1").unwrap(), b"GACCT123");
        assert_eq!(c.decrypt(&env, b"row-2"), Err(CryptoError::DecryptFailed));
        let mut t = env.clone();
        t.ciphertext[0] ^= 1;
        assert_eq!(c.decrypt(&t, b"row-1"), Err(CryptoError::DecryptFailed));
        assert_eq!(Envelope::decode(&env.encode()).unwrap(), env);
    }

    #[test]
    fn rotation_keeps_old_and_new_versions_readable() {
        let c = cipher();
        let old = c.encrypt(b"x", b"a").unwrap();
        c.provider().rotate().unwrap();
        let new = c.encrypt(b"y", b"b").unwrap();
        assert_eq!((old.key_version, new.key_version), (1, 2));
        assert_eq!(c.decrypt(&old, b"a").unwrap(), b"x");
        assert_eq!(c.status([&old, &new]).remaining_on_old_versions(), 1);
        let re = c.rewrap(&old).unwrap();
        assert_eq!(re.key_version, 2);
        assert_eq!(c.rewrap(&re).unwrap(), re);
        assert_eq!(c.decrypt(&re, b"a").unwrap(), b"x");
    }

    #[test]
    fn debug_output_hides_key_material() {
        let c = cipher();
        let env = c.encrypt(b"secret", b"a").unwrap();
        assert!(!format!("{env:?} {:?}", c.provider()).contains("secret"));
    }
}
