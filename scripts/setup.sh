#!/usr/bin/env bash
# scripts/setup.sh — Synapse Core developer setup script
set -euo pipefail

# ─── Colors ───────────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
RESET='\033[0m'

# ─── Helpers ──────────────────────────────────────────────────────────────────
info()    { echo -e "${BLUE}[INFO]${RESET}  $*"; }
success() { echo -e "${GREEN}[OK]${RESET}    $*"; }
warn()    { echo -e "${YELLOW}[WARN]${RESET}  $*"; }
error()   { echo -e "${RED}[ERROR]${RESET} $*" >&2; }
die()     { error "$*"; exit 1; }

header() {
  echo ""
  echo -e "${BOLD}${BLUE}══════════════════════════════════════════${RESET}"
  echo -e "${BOLD}${BLUE}  $*${RESET}"
  echo -e "${BOLD}${BLUE}══════════════════════════════════════════${RESET}"
}

# ─── Flags ────────────────────────────────────────────────────────────────────
RESET_FLAG=false
for arg in "$@"; do
  case "$arg" in
    --reset) RESET_FLAG=true ;;
    --help|-h)
      echo "Usage: $0 [--reset]"
      echo ""
      echo "  --reset   Wipe existing Docker volumes and recreate everything from scratch"
      exit 0
      ;;
    *) die "Unknown argument: $arg" ;;
  esac
done

# ─── Prerequisite checks ──────────────────────────────────────────────────────
header "Checking prerequisites"

check_command() {
  local cmd="$1"
  local label="${2:-$1}"
  if command -v "$cmd" &>/dev/null; then
    success "$label found: $(command -v "$cmd")"
  else
    die "$label is required but not installed. See docs/setup.md for installation instructions."
  fi
}

check_command rustc  "Rust (rustc)"
check_command cargo  "Cargo"
check_command docker "Docker"
check_command psql   "psql (PostgreSQL client)"

# Verify Docker daemon is running
if ! docker info &>/dev/null; then
  die "Docker daemon is not running. Please start Docker and try again."
fi
success "Docker daemon is running"

# ─── Reset (optional) ─────────────────────────────────────────────────────────
if [ "$RESET_FLAG" = true ]; then
  header "Resetting environment (--reset)"
  warn "This will destroy all local data volumes."
  read -r -p "$(echo -e "${YELLOW}Are you sure? [y/N]:${RESET} ")" confirm
  if [[ "$confirm" =~ ^[Yy]$ ]]; then
    info "Stopping and removing containers + volumes..."
    docker compose down -v --remove-orphans 2>/dev/null || true
    success "Environment wiped"
  else
    info "Reset cancelled."
  fi
fi

# ─── Environment file ─────────────────────────────────────────────────────────
header "Environment configuration"

if [ ! -f .env ]; then
  if [ -f .env.example.failover ]; then
    cp .env.example.failover .env
    success "Copied .env.example.failover → .env"
  else
    # Fallback: write a minimal .env
    cat > .env <<'EOF'
SERVER_PORT=3000
DATABASE_URL=postgres://synapse:synapse@localhost:5432/synapse
STELLAR_HORIZON_URL=https://horizon-testnet.stellar.org
EOF
    success "Created minimal .env"
  fi
  warn "Review .env and update any values before running in production."
else
  info ".env already exists — skipping copy"
fi

# ─── Start Docker services ────────────────────────────────────────────────────
header "Starting Docker services"

info "Running: docker compose up -d postgres redis"
docker compose up -d postgres redis
success "Docker services started"

# ─── Wait for services to be healthy ─────────────────────────────────────────
header "Waiting for services to be healthy"

wait_healthy() {
  local container="$1"
  local label="${2:-$container}"
  local max_attempts=30
  local attempt=0

  info "Waiting for $label..."
  until [ "$(docker inspect --format='{{.State.Health.Status}}' "$container" 2>/dev/null)" = "healthy" ]; do
    attempt=$((attempt + 1))
    if [ "$attempt" -ge "$max_attempts" ]; then
      die "$label did not become healthy after ${max_attempts} attempts. Check: docker logs $container"
    fi
    sleep 2
    echo -n "."
  done
  echo ""
  success "$label is healthy"
}

wait_healthy "synapse-postgres" "PostgreSQL"
wait_healthy "synapse-redis"    "Redis"

# ─── Run migrations ───────────────────────────────────────────────────────────
header "Running database migrations"

# Ensure sqlx-cli is available
if ! command -v sqlx &>/dev/null; then
  info "sqlx-cli not found — installing..."
  cargo install sqlx-cli --no-default-features --features postgres
  success "sqlx-cli installed"
else
  success "sqlx-cli found: $(command -v sqlx)"
fi

# Load DATABASE_URL from .env if not already set
if [ -z "${DATABASE_URL:-}" ]; then
  # shellcheck disable=SC1091
  set -a; source .env; set +a
fi

info "Running: sqlx migrate run"
sqlx migrate run
success "Migrations applied"

# ─── Health check ─────────────────────────────────────────────────────────────
header "Verifying setup"

SERVER_PORT="${SERVER_PORT:-3000}"

# Check if the app is already running (e.g. via docker compose)
if curl -sf "http://localhost:${SERVER_PORT}/health" &>/dev/null; then
  success "Health check passed — service is up at http://localhost:${SERVER_PORT}"
else
  info "App is not running yet. Start it with:"
  echo -e "    ${BOLD}cargo run${RESET}"
  echo -e "  or:"
  echo -e "    ${BOLD}docker compose up --build${RESET}"
  echo ""
  info "Then verify with:"
  echo -e "    ${BOLD}curl http://localhost:${SERVER_PORT}/health${RESET}"
fi

# ─── Done ─────────────────────────────────────────────────────────────────────
header "Setup complete"
success "Synapse Core development environment is ready."
echo ""
echo -e "  ${BOLD}Next steps:${RESET}"
echo -e "    cargo run                          # start the server"
echo -e "    docker compose up --build          # full stack via Docker"
echo -e "    docker compose -f docker-compose.dev.yml up  # hot-reload dev mode"
echo ""
