# Envelope encryption

`src/crypto/`. Per-record AES-256-GCM DEK, wrapped by a KEK from a `KeyProvider` (`LocalKeyProvider` for dev, `KmsKeyProvider<C: KmsClient>` for external KMS/HSM). Select with `KEYSTORE_PROVIDER=local|kms` (`KEYSTORE_LOCAL_KEK`, `KEYSTORE_KMS_KEY_ID`) via `config::keystore_config()`.

Envelope text format: `v1.<key_version>.<wrapped_dek>.<nonce>.<ciphertext>`, stored in `*_enc` columns with `enc_key_version`. AAD should be the row id.

Rotation: `provider.rotate()` adds a version; `EnvelopeCipher::rewrap` lazily re-wraps (idempotent, resumable, payload untouched); `status()` reports records remaining on old versions. Decrypt failures return `CryptoError`, never panic or empty. Key material is redacted from `Debug`.

Migration (expand/contract): `*_enc` columns are added; backfill and verify, then drop plaintext columns in a later migration.
