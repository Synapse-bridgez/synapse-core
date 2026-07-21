# Build stage
FROM rust:latest AS builder 
WORKDIR /app

# Copy manifests and lockfile
COPY Cargo.toml Cargo.lock ./

# Copy source and migrations
COPY src ./src
COPY migrations ./migrations

# Build the application
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim
# postgresql-14 (client + server binaries) pulled from the PGDG repo, pinned
# to match the postgres:14-alpine image in docker-compose.yml: PITR restore
# (scripts/pitr_restore.sh) needs pg_basebackup/pg_ctl/postgres/psql on PATH,
# and physical-restore tooling should match the source server's major version.
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates wget gnupg lsb-release \
    && install -d /usr/share/postgresql-common/pgdg \
    && wget -qO /usr/share/postgresql-common/pgdg/apt.postgresql.org.asc \
      https://www.postgresql.org/media/keys/ACCC4CF8.asc \
    && echo "deb [signed-by=/usr/share/postgresql-common/pgdg/apt.postgresql.org.asc] http://apt.postgresql.org/pub/repos/apt $(lsb_release -cs)-pgdg main" \
      > /etc/apt/sources.list.d/pgdg.list \
    && apt-get update && apt-get install -y --no-install-recommends postgresql-14 \
    && rm -rf /var/lib/apt/lists/*
ENV PATH="/usr/lib/postgresql/14/bin:${PATH}"
WORKDIR /app
COPY --from=builder /app/target/release/synapse-core /app/synapse-core
COPY --from=builder /app/migrations ./migrations
EXPOSE 3000
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD wget -qO- http://localhost:3000/health || exit 1
CMD ["/app/synapse-core"]






