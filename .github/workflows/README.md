# GitHub Actions CI/CD

This directory contains the GitHub Actions workflow definitions for Synapse Core.
The primary pipeline is [`rust.yml`](./rust.yml), which runs formatting, migration
safety checks, clippy, builds, unit tests, integration tests, coverage collection,
and coverage threshold enforcement.

## Idempotency Keys

In the CI/CD pipeline, an idempotency key is any deterministic value GitHub
Actions uses to identify reusable work across retries or repeated runs. The
current workflow uses these keys for Cargo registry and build artifact caching.
They are separate from runtime `X-Idempotency-Key` headers used by the API; see
[`docs/idempotency.md`](../../docs/idempotency.md) for webhook request behavior.

This document covers GitHub Actions behavior only. Artifact names such as
`coverage-report`, Codecov upload metadata, and database service names are not
idempotency keys because they do not control reuse of previously computed work.

### Current Keys

The workflow defines cache keys with `actions/cache@v4`:

```yaml
key: ${{ runner.os }}-${{ matrix.rust }}-cargo-${{ hashFiles('**/Cargo.lock') }}
restore-keys: |
  ${{ runner.os }}-${{ matrix.rust }}-cargo-
  ${{ runner.os }}-cargo-
```

Coverage uses a separate namespace:

```yaml
key: ${{ runner.os }}-coverage-cargo-${{ hashFiles('**/Cargo.lock') }}
restore-keys: |
  ${{ runner.os }}-coverage-cargo-
  ${{ runner.os }}-cargo-
```

These keys are intentionally stable for the same operating system, Rust
toolchain, job purpose, and dependency lockfile. Re-running a failed job can
reuse the same cache without repeating dependency downloads, while lockfile
changes naturally create a new key.

### Design Rules

- Include the runner OS in cache keys so Linux, macOS, and Windows artifacts do
  not collide.
- Include the Rust toolchain or job purpose when artifacts can differ between
  jobs.
- Include `hashFiles('**/Cargo.lock')` for dependency-sensitive caches so stale
  crates are not reused after dependency updates.
- Use `restore-keys` only from most-specific to least-specific prefixes. This
  preserves performance while allowing safe fallback to older compatible caches.
- Keep keys deterministic. Do not include timestamps, random values, commit SHAs,
  or run IDs unless the cache must be intentionally single-use.
- Do not place secrets, API keys, tokens, database URLs, branch names containing
  sensitive data, or user-supplied payloads in keys. Cache keys are visible in
  workflow logs and GitHub cache metadata.

### Retry And Concurrency Behavior

GitHub Actions cache writes are immutable for a given key. If two jobs compute
the same key, the first successful save wins and later saves are skipped. This is
expected and safe for the current Cargo cache usage because dependencies are
derived from `Cargo.lock` and build outputs are only used as performance hints.

CI jobs must remain correct when a cache is missed, stale, or not saved. The
pipeline always runs `cargo fmt`, migration safety checks, `cargo clippy`,
`cargo build`, `cargo test`, and coverage commands against the checked-out
source, so cache reuse cannot bypass verification.

### Security And Performance Assumptions

- Cache contents must be treated as untrusted optimization data. Builds and tests
  must continue to compile and validate the workspace after restore.
- Cache paths are limited to Cargo registry, Cargo git database, and `target/`.
  Do not cache `.env`, generated credentials, coverage upload tokens, database
  dumps, or other secret-bearing files.
- The lockfile hash bounds cache growth by invalidating only when dependencies
  change. The `SCCACHE_CACHE_SIZE=1G` environment variable bounds compiler cache
  usage during the job.
- The workflow grants `id-token: write` only to the coverage job because Codecov
  upload may require OIDC. Other jobs run with default read-only source access.

### Change Checklist

When editing `.github/workflows/rust.yml`:

- Keep cache keys deterministic and secret-free.
- Add a new namespace when a cache serves a materially different job or tool.
- Prefer dependency or configuration hashes over commit-specific keys.
- Verify a cold run still succeeds if every cache restore misses.
- Run `cargo test` locally before committing workflow documentation or behavior
  changes.

### Validation

For documentation-only changes, verify that the workflow still matches this
contract by checking the cache keys in `.github/workflows/rust.yml` and running:

```sh
cargo test
```

For workflow behavior changes, also trigger a pull request run and confirm that
the pipeline passes when cache restore misses occur. Cache misses should make the
run slower, not less correct.
