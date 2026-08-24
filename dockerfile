# Build stage
FROM rust:latest AS builder 
WORKDIR /app

# Copy manifests and lockfile
COPY Cargo.toml Cargo.lock ./

# Copy source, migrations, and benches. `benches/` is required even for a
# non-bench `cargo build --release`: Cargo.toml declares a [[bench]] target
# (critical_paths), and cargo validates every declared target's path exists
# while parsing the manifest, regardless of which command you run. Without
# this, the image build fails at Step "RUN cargo build --release" with
# "can't find `critical_paths` bench" before ever reaching application code.
COPY src ./src
COPY migrations ./migrations
COPY benches ./benches

# Build the application
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates wget && rm -rf /var/lib/apt/lists/*

# Fixed UID/GID (not the next-available one useradd would pick) so the
# numeric owner is stable and reproducible across image rebuilds, and
# doesn't collide with a host UID via a bind mount.
RUN groupadd --gid 10001 synapse && \
    useradd --uid 10001 --gid synapse --no-create-home --shell /usr/sbin/nologin synapse

WORKDIR /app
COPY --from=builder --chown=synapse:synapse /app/target/release/synapse-core /app/synapse-core
COPY --from=builder --chown=synapse:synapse /app/migrations ./migrations

# WORKDIR creates /app as root, before the two COPY --chown above touch only
# the specific paths they copy. Config::backup_dir defaults to the relative
# path "./backups" (BackupService creates it on demand, not created here) —
# without this, /app itself stays root-owned (mode 755, not writable by a
# non-owner), so the app would fail to create that directory the first time
# a backup runs under the non-root user below. Chowning the directory itself
# (not -R; nothing else is written here) covers that and any other
# relative-path subdirectory the app creates under its own CWD at runtime.
RUN chown synapse:synapse /app

USER synapse:synapse

EXPOSE 3000
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD wget -qO- http://localhost:3000/health || exit 1
CMD ["/app/synapse-core"]






