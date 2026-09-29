# Rust CI Cache Strategy

Rust workflows share the root workspace `target/` directory and Cargo
registry/git caches using a common key derived from the root lockfile, workspace
manifests, and Rust sources.
`sccache` is enabled consistently, including in the CLI, SDK, workspace, and
webhook workflows. This lets unchanged compiler work be reused even when an
exact artifact-cache key is not available. Crate-specific format checks run in
parallel with clippy/test jobs; the core unit/integration and workspace smoke
jobs were already independent.

Hosted Actions cache-hit history is not available from a checkout. Review the
`actions/cache` restore result and `sccache --show-stats` output in workflow
logs to establish hit rates on subsequent runs. No historical rate is claimed
by this change.