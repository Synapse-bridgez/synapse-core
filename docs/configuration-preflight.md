# Configuration Validation and Deployment Preflight

Implements #1517. Code: `src/config/preflight.rs`.

## Startup (fail fast)

`Config::load` runs `preflight::validate_env` after the profile `.env` files and
defaults are applied. If anything is wrong, startup aborts with **one report
listing every problem**, not just the first:

```
configuration preflight failed with 3 problem(s):
  - [required] ANCHOR_WEBHOOK_SECRET is not set
  - [type] SERVER_PORT must be a number (got "abc")
  - [consistency] DB_MIN_CONNECTIONS (50) must be <= DB_MAX_CONNECTIONS (10)
```

Checks:

- **required** - `STELLAR_HORIZON_URL`; `DATABASE_URL` and `ANCHOR_WEBHOOK_SECRET`
  unless Vault is configured (then `DATABASE_URL_TEMPLATE` or `DATABASE_URL`).
- **format** - URL schemes for database, Redis, Horizon and OTLP URLs;
  `LOG_FORMAT`, `APP_ENV`; `DATABASE_URL_TEMPLATE` contains `{password}`.
- **type / range** - numeric settings parse and fall in sane bounds.
- **consistency** - `VAULT_ROLE_ID`/`VAULT_SECRET_ID` set together;
  `DB_MIN_CONNECTIONS <= DB_MAX_CONNECTIONS`;
  `DB_TIMEOUT_READ_SECS <= DB_TIMEOUT_WRITE_SECS <= DB_TIMEOUT_ADMIN_SECS`;
  `PROCESSOR_MIN_BATCH <= PROCESSOR_BATCH_SIZE <= PROCESSOR_MAX_BATCH`;
  `PARTITION_LOOKAHEAD_MIN <= PARTITION_LOOKAHEAD_MAX`;
  `SETTLEMENT_MIN_TX_COUNT <= SETTLEMENT_MAX_BATCH_SIZE`.

Secret values (database/Redis URLs, webhook secret, backup key, Vault
credentials) are never printed.

## Preflight command

```bash
synapse-core preflight                  # config + reachability + migration drift
synapse-core preflight --offline        # config only (offline CI)
synapse-core preflight --timeout-secs 3
```

Exits non-zero on any problem. It is read-only and safe against production:
one `SELECT 1`, one Redis `PING`, one `GET` of the Horizon root, and a `SELECT`
on `_sqlx_migrations`, each bounded by the timeout. It also prints whether
each secret is present or absent.

## Migration drift

`AUTO_MIGRATE` (default `true`) controls whether `serve` applies pending
migrations. With `AUTO_MIGRATE=false`, `serve` refuses to start while
migrations are pending, and `preflight` reports them as a failure; run
`synapse-core db migrate` first.
