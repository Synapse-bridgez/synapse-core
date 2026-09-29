# Docker Compose parity review

This document records the drift review between the production stack in `docker-compose.yml` and the local dev/load variants (`docker-compose.dev.yml`, `docker-compose.load.yml`). The goal is to keep local reproduction and load testing representative without changing production defaults in place.

## Production baseline

The production compose file defines:

- Postgres 14 alpine
- Redis 7 alpine
- Vault dev server
- App container built from the repo
- `DATABASE_URL_TEMPLATE`, `BACKUP_DATABASE_URL`, and `VAULT_*` environment variables
- no explicit CPU/memory caps on the app

## Diff summary

| Area | Production | Dev | Load | Status |
| --- | --- | --- | --- | --- |
| Postgres image | `postgres:14-alpine` | same | same | aligned |
| Redis image | `redis:7-alpine` | same | same | aligned |
| Vault | included | missing before | missing before | gap closed |
| `DATABASE_URL_TEMPLATE` | present | missing | missing | gap closed |
| `BACKUP_DATABASE_URL` | present | missing | missing | gap closed |
| `VAULT_ADDR` / `VAULT_ROLE_ID` / `VAULT_SECRET_ID` | present | missing | missing | gap closed |
| DB connection defaults | `DB_*` from app config | relaxed dev defaults | tuned load defaults | intentional |
| App resources | unbounded | debug-friendly limits | `2 CPU / 1G RAM` | intentional |
| Debug networking | app port only | dev includes `9229` debug port | default app port only | intentional |

## Changes made

1. Added `vault` to the dev and load compose files so they match the production dependency graph and secret-access expectations.
2. Added the same runtime environment variables used in production for database templating, backup connection, and Vault wiring.
3. Added the DB configuration variables the app reads at runtime (`DB_MIN_CONNECTIONS`, `DB_MAX_CONNECTIONS`, etc.) so local runs mirror production expectations.
4. Kept the intentionally different operational settings explicit:
   - `docker-compose.dev.yml` keeps a local debug toolchain and relaxed defaults for fast iteration.
   - `docker-compose.load.yml` keeps higher load-oriented DB settings and resource caps for reproducible benchmarking.

## Deliberate differences still allowed

- Dev still runs the Rust app in a watch container and exposes a debugger port.
- Load keeps a dedicated `k6` profile and a `max_connections=200` Postgres tuning profile to stress throughput rather than mimic production as-is.
- Production intentionally stays unbounded by Docker resource caps; those are a deployment-layer concern and are not changed here.

This keeps parity for credentials, dependency wiring, and runtime config while preserving the two scenarios that are intentionally different: local iteration and load testing.
