# Changelog

All notable changes to `synapse-cli` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows the policy described in [VERSIONING.md](./VERSIONING.md).

## [Unreleased]

### Fixed

- **Breaking (bug fix):** every `admin ...` subcommand, `stats ...`, and
  `admin webhooks health*` sent `X-API-Key` for authentication, but the
  server's `admin_auth` middleware only ever checks `Authorization: Bearer
  <token>` — these commands failed with `401` against any real server
  regardless of what credential was supplied. They now send
  `Authorization: Bearer <api-key>` via a new shared `AdminClient`. If you
  were working around this by calling the server directly with the correct
  header, switch back to the CLI now that it authenticates correctly.
- **Breaking (bug fix):** `transactions export` requested
  `/transactions/export`, a path the server has never served (the real
  route is `/export`), and sent no credential at all even though `/export`
  requires admin auth. It now calls `GET /export` with
  `Authorization: Bearer <api-key>`.
- **Breaking (bug fix):** `graphql query` sent no credential at all, even
  though `/graphql` requires admin auth. It now sends
  `Authorization: Bearer <api-key>` and reads `--api-key`/`SYNAPSE_API_KEY`
  (previously `graphql query` accepted neither).
- The CLI's differentiated exit codes (`EXIT_AUTH_FAILURE=2`,
  `EXIT_NOT_FOUND=3`, defined in `error.rs` since before this release) were
  never wired into `main.rs`'s error handling — every command failure exited
  `1` regardless of cause. `main.rs` now maps the real error (an
  `Authorization`/`404` failure surfaced by the HTTP layer) onto the correct
  code; see the [Exit codes](./README.md#exit-codes) section of the README.
- `mock-server.rs` (this crate's own test double) never validated any auth
  header for any route, so the CLI's test suite passed regardless of which
  header name each client sent — a permissive test harness in exactly the
  dimension that mattered. It now validates `Authorization: Bearer <token>`
  for every route behind `admin_auth` on the real server, matching real
  server behavior.

### Added

- Initial crate scaffold: `Cargo.toml`, `src/main.rs`, `src/lib.rs`.
- `clap`-based CLI entry point with top-level subcommand dispatch.
- `synapse workspace` command group: `list`, `get`, `create`, `delete`.
- `synapse event` command group: `list`, `get`, `publish`.
- `synapse subscription` command group: `list`, `get`, `create`, `cancel`.
- `synapse admin reconciliation` command group: `list`, `get`, `trigger`.
- Output formatters: `--output table` (default), `--output json`, `--output csv`.
- Global flags: `--api-url`, `--token` (env: `SYNAPSE_TOKEN`), `--verbose`.
- Exit-code contract: 0 success, 1 API error, 2 usage/config error.
- Integration tests using `assert_cmd` and `predicates`.
- Wired into the root workspace (`Cargo.toml` `[workspace] members`).
- Scoped CI workflow (`.github/workflows/cli-synapse-ci.yml`): fmt, clippy, tests.
