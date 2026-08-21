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

# Runtime stage (unchanged)
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates wget && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/synapse-core /app/synapse-core
COPY --from=builder /app/migrations ./migrations
EXPOSE 3000
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD wget -qO- http://localhost:3000/health || exit 1
CMD ["/app/synapse-core"]






