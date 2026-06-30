# Webhook Handlers in CI/CD

This document explains how webhook handler logic is tested within the GitHub
Actions workflow (`.github/workflows/webhook.yml`).

## Workflow overview

```
repository_dispatch / workflow_dispatch
        │
        ├── unit-tests            (no services)
        │       ├── cargo test --lib webhook
        │       └── cargo test --test webhook_{auth,delivery,test}
        │
        └── integration-tests     (Postgres + Redis, parallel with unit-tests)
                ├── cargo test --test webhook_replay_test -- --ignored
                └── cargo test --test webhook_concurrent_delivery_test -- --ignored
```

Both jobs run in parallel. Workflow-level concurrency cancels stale runs on the
same branch so duplicate webhook events do not repeat expensive work.

## Where webhook tests live

| Layer | Source file | Test location | Job |
|---|---|---|---|
| Payload validation | `src/handlers/webhook.rs` | inline `#[cfg(test)]` | unit-tests |
| Cache signature checks | `src/cache/webhook.rs` | inline `#[cfg(test)]` | unit-tests |
| Outgoing dispatcher filters | `src/services/webhook_dispatcher.rs` | inline `#[cfg(test)]` | unit-tests |
| Telemetry webhook security | `src/telemetry/webhook.rs` | inline `#[cfg(test)]` | unit-tests |
| JSON schema validation | `src/validation/schemas.rs` | inline `#[cfg(test)]` | unit-tests |
| Admin replay serialization | `src/handlers/admin/webhook_replay.rs` | inline `#[cfg(test)]` | unit-tests |
| HMAC signature primitives | — | `tests/webhook_auth_test.rs` | unit-tests |
| Delivery retry behavior | — | `tests/webhook_delivery_test.rs` | unit-tests |
| Callback payload structure | — | `tests/webhook_test.rs` | unit-tests |
| Replay history tracking | `src/handlers/admin/webhook_replay.rs` | `tests/webhook_replay_test.rs` | integration-tests |
| Exactly-once delivery + DLQ | `src/services/webhook_dispatcher.rs` | `tests/webhook_concurrent_delivery_test.rs` | integration-tests |

## Job 1 — unit-tests

Runs `cargo test --lib webhook` plus stateless integration test binaries. No
Postgres or Redis services are provisioned.

Webhook logic covered:

- **Inbound payload validation** — rejects unknown fields, invalid Stellar
  addresses, malformed amounts, SQL-injection-like strings, and overlong fields.
- **HMAC signature verification** — constant-time comparison, timestamp window
  checks, and replay cache key construction.
- **Outgoing dispatcher** — filter rules (asset codes, min amount), signature
  versioning, and deterministic signing.
- **Telemetry webhook handler** — rejects oversized payloads, stale timestamps,
  and invalid signatures before processing.

## Job 2 — integration-tests

Runs database-backed webhook tests. Requires live Postgres and Redis service
containers plus applied migrations.

### What is tested end-to-end

**Webhook replay** (`tests/webhook_replay_test.rs`):
- Replay history rows are written to `webhook_replay_history`.
- Failed webhook listing queries return expected audit-log records.

**Concurrent delivery** (`tests/webhook_concurrent_delivery_test.rs`):
- Exactly-once delivery under concurrent dispatcher workers.
- Exhausted retries route deliveries to the DLQ with attempt history.
- Per-endpoint circuit breaker isolates failing endpoints.

The concurrent delivery tests use testcontainers for isolated Postgres and Redis
instances in addition to the workflow service containers used by replay tests.

## Performance optimizations

- **Parallel jobs** — unit and integration tests run concurrently instead of in
  a single sequential job.
- **sccache** — compiler output is cached via `RUSTC_WRAPPER=sccache` with a
  1 GiB bound (`SCCACHE_CACHE_SIZE=1G`).
- **Cargo artifact cache** — registry, git, and `target/` directories are
  restored from a webhook-specific namespace with fallback to the main CI cache.
- **No redundant build step** — `cargo test` compiles and runs in one pass.
- **Concurrency groups** — stale webhook runs on the same branch are cancelled;
  `taiki-e/install-action` calls are serialized to avoid GitHub API rate limits.

## Security assumptions

- **Minimal permissions** — both jobs use `permissions: contents: read`.
- **Event type validation** — the workflow rejects empty or malformed event
  types before any build steps. Allowed characters match
  `cache::webhook::validate_event_id`: `[A-Za-z0-9_:-]` with a 64-character
  ceiling.
- **Untrusted cache** — restored Cargo caches are treated as optimization-only
  data. Tests always compile and run against the checked-out source.
- **Runner-local credentials** — Postgres and Redis passwords are disposable
  test values for service containers, not production secrets.
- **No secret logging** — client payloads from `repository_dispatch` are not
  written to workflow logs.

## Cache keys

Webhook jobs use a dedicated namespace that can fall back to the main Rust CI
cache:

```yaml
key: ${{ runner.os }}-webhook-cargo-${{ hashFiles('**/Cargo.lock') }}
restore-keys: |
  ${{ runner.os }}-webhook-cargo-
  ${{ runner.os }}-stable-cargo-
  ${{ runner.os }}-cargo-
```

## Change checklist

When editing `.github/workflows/webhook.yml`:

- Keep cache keys deterministic and secret-free.
- Confirm both jobs succeed from a cold session with no cache restore.
- Confirm event type validation still rejects injection-like input.
- Run the webhook test commands locally before committing:

```sh
cargo test --lib webhook
cargo test --test webhook_auth_test --test webhook_delivery_test --test webhook_test
DATABASE_URL=postgres://synapse:synapse@localhost:5432/synapse_test \
  cargo test --test webhook_replay_test -- --ignored
cargo test --test webhook_concurrent_delivery_test -- --ignored
```

For workflow behavior changes, also trigger a `workflow_dispatch` run and
confirm that cache misses make the run slower, not less correct.
