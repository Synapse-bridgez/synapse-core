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
-- Precondition: this app has no tenant-provisioning HTTP endpoint, so
-- `tenants` is expected to be empty at migration time in every environment
-- that has run this far. That expectation is enforced below, not just
-- documented, because a deployment that already has real tenant rows would
-- otherwise get a silent, errorless "success" that actually breaks API-key
-- auth (old plaintext keys never hash-match again) and webhook-signature
-- verification (old plaintext secrets aren't valid PGP ciphertext) for every
-- existing tenant. If this fires, do not remove the guard to "fix" it --
-- rotate api_key and re-encrypt webhook_secret for existing tenants through
-- the new code path first, then adapt this migration to do the conversion
-- explicitly instead of assuming an empty table.
DO $$
DECLARE
    tenant_count INTEGER;
BEGIN
    SELECT count(*) INTO tenant_count FROM tenants;
    IF tenant_count > 0 THEN
        RAISE EXCEPTION 'tenants has % existing row(s); this migration assumes tenants is empty and would silently break API-key auth and webhook-signature verification for every existing tenant. See the comment at the top of this migration before proceeding.', tenant_count;
    END IF;
END
$$;

ALTER TABLE tenants RENAME COLUMN api_key TO api_key_hash;

-- The guard above proves this table is empty at this point, so the cast
-- below never actually reinterprets real secret bytes -- it only changes
-- the column's declared type. Every row written after this migration goes
-- through pgp_sym_encrypt (see get_all_tenant_configs / tenant insert sites
-- in src/db/queries.rs), never through this cast.
ALTER TABLE tenants ALTER COLUMN webhook_secret DROP DEFAULT;
ALTER TABLE tenants ALTER COLUMN webhook_secret TYPE BYTEA USING webhook_secret::bytea;
