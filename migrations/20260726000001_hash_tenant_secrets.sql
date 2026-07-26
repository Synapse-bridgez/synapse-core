-- Stop storing tenant credentials as plaintext.
--
-- api_key: renamed to api_key_hash and now holds an HMAC-SHA256 digest keyed
-- by the server-side TENANT_SECRET_KEY pepper (see hash_api_key in
-- src/db/queries.rs), instead of the raw bearer credential. A stolen copy of
-- this table no longer yields a usable API key.
--
-- webhook_secret: now stored as a pgcrypto-encrypted (pgp_sym_encrypt) BYTEA
-- blob rather than plaintext, and decrypted on read via pgp_sym_decrypt using
-- the same TENANT_SECRET_KEY. It remains recoverable, which is required
-- since it is the HMAC key used to sign/verify webhook payloads.
--
-- This app has no tenant-provisioning code path outside tests, so the
-- tenants table is expected to be empty at migration time. A deployment
-- that already has tenant rows must rotate api_key and re-encrypt
-- webhook_secret through the new code path.

ALTER TABLE tenants RENAME COLUMN api_key TO api_key_hash;

ALTER TABLE tenants ALTER COLUMN webhook_secret DROP DEFAULT;
ALTER TABLE tenants ALTER COLUMN webhook_secret TYPE BYTEA USING webhook_secret::bytea;
