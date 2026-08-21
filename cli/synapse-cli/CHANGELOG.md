# Changelog

All notable changes to `synapse-cli` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows the policy described in [VERSIONING.md](./VERSIONING.md).

## [Unreleased]

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
