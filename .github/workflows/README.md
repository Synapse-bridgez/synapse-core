# GitHub Actions CI/CD

This directory contains the GitHub Actions workflow definitions for Synapse Core.
The primary pipeline is [`rust.yml`](./rust.yml), which runs formatting, migration
safety checks, clippy, builds, unit tests, integration tests, coverage collection,
and coverage threshold enforcement.

## Session Management

In this CI/CD module, a session is one isolated GitHub Actions job execution:
`unit-tests`, `integration-tests`, or `coverage`. Session state includes the
checked-out source, job-scoped environment variables, service containers,
restored caches, generated build outputs, coverage files, and any temporary
credentials exposed by GitHub Actions for that job.

Application runtime sessions, user authentication state, webhook idempotency
state, and Redis data created by tests are outside the CI/CD session boundary
except when a job provisions local Postgres or Redis services to exercise them.

### Session Lifecycle

Each job follows the same lifecycle:

1. GitHub creates a fresh `ubuntu-latest` runner.
2. The workflow checks out the repository with `actions/checkout@v4`.
3. The job installs its Rust toolchain and required test tools.
4. `actions/cache@v4` restores Cargo registry, Cargo git, and `target/` data
   as optimization-only state.
5. Job-local Postgres and Redis service containers are started when required.
6. Migrations, formatting, linting, builds, tests, and coverage commands run
   against the checked-out source.
7. Coverage artifacts are uploaded only from the `coverage` job.
8. The runner, service containers, environment variables, and temporary files
   are discarded when the job ends.

Jobs do not share live processes, database contents, Redis data, environment
variables, or generated credentials. The only intentional cross-run state is the
GitHub Actions cache described in [Idempotency Keys](#idempotency-keys) and the
published coverage artifact.

### Job Boundaries

- `unit-tests` owns formatting, migration safety, clippy, build, and library/bin
  unit tests. It provisions Postgres because migrations and SQL-backed code paths
  need a database session.
- `integration-tests` owns ignored integration tests. It provisions both
  Postgres and Redis so tests can cover database and cache/session behavior
  without reaching shared infrastructure.
- `coverage` depends on successful unit and integration jobs, then creates a new
  runner session to collect and upload coverage. It does not reuse databases,
  Redis data, or build processes from earlier jobs.

### Security Rules

- Treat every restored cache as untrusted input. A session must compile, lint,
  migrate, and test the checked-out source after cache restore.
- Keep secrets out of cache paths, artifact paths, job names, cache keys, and log
  output. The current cache paths are limited to Cargo registry data, Cargo git
  data, and `target/`.
- Grant elevated permissions only to jobs that need them. The workflow grants
  `id-token: write` only to `coverage` for Codecov OIDC support; other jobs use
  default source read access.
- Use job-local service credentials only for disposable CI services. The
  Postgres password in `rust.yml` is a test credential for the runner-local
  container, not an environment credential.
- Do not persist `.env` files, database dumps, Redis snapshots, API keys,
  coverage upload tokens, or generated credentials as artifacts or caches.
- Keep session-specific data out of deterministic cache keys. Cache keys are
  visible in workflow logs and GitHub cache metadata.

### Performance Rules

- Prefer session-local services over shared CI databases or Redis instances.
  This keeps tests isolated and avoids cleanup races between parallel jobs.
- Cache only dependency and build data that can be safely regenerated. Cache
  misses should slow the session down, not change its result.
- Keep cache namespaces aligned with job purpose. Coverage uses a separate cache
  namespace because coverage instrumentation can produce different build output
  than normal test jobs.
- Bound compiler cache growth with `SCCACHE_CACHE_SIZE=1G`.

### Validation Checklist

When changing CI/CD session behavior:

- Confirm every job still succeeds from a cold session with no cache restore.
- Confirm local service containers expose only runner-local test credentials.
- Confirm no new artifact or cache path can contain secrets or production data.
- Confirm any new job permission is scoped to the single job that requires it.
- Run `cargo test` before committing, and trigger a pull request workflow run for
  behavior changes.

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
