#!/bin/bash
# Quick setup script for Synapse Core development environment

set -e

echo "🚀 Setting up Synapse Core development environment..."

# Check if Docker is installed
if ! command -v docker &> /dev/null; then
    echo "❌ Docker is not installed. Please install Docker first: https://docs.docker.com/get-docker/"
    exit 1
fi

# Check if Rust is installed
if ! command -v cargo &> /dev/null; then
    echo "❌ Rust is not installed. Please install Rust: https://rustup.rs/"
    exit 1
fi

# Check PostgreSQL version requirement
echo "📋 Checking PostgreSQL version requirement..."
MIN_PG_VERSION=14
echo "   ℹ️  PostgreSQL 14+ is required for partitioning features"

# Create .env file if it doesn't exist
if [ ! -f .env ]; then
    echo "📝 Creating .env file from .env.example..."
    cp .env.example .env
    echo "   ✅ .env created. Update it with your database credentials if needed."
fi

# Start PostgreSQL container if not running
PG_CONTAINER="synapse-postgres"
if ! docker ps | grep -q "$PG_CONTAINER"; then
    echo "🐘 Starting PostgreSQL container..."
    docker run --name "$PG_CONTAINER" \
        -e POSTGRES_USER=synapse \
        -e POSTGRES_PASSWORD=synapse \
        -e POSTGRES_DB=synapse \
        -p 5432:5432 \
        -d postgres:14-alpine
    
    echo "   ⏳ Waiting for PostgreSQL to be ready..."
    sleep 5
    
    # Verify connection
    docker exec "$PG_CONTAINER" pg_isready -U synapse || {
        echo "   ❌ PostgreSQL failed to start. Check logs: docker logs $PG_CONTAINER"
        exit 1
    }
    echo "   ✅ PostgreSQL is ready!"
else
    echo "✅ PostgreSQL container is already running"
fi

# Load environment variables
export $(cat .env | grep -v '^#' | xargs)

# Run migrations (if DATABASE_URL is set)
if [ -n "$DATABASE_URL" ]; then
    echo "🔄 Running database migrations..."
    cargo sqlx migrate run || echo "   ⚠️  Migration command failed. Migrations will run on app startup."
fi

echo ""
echo "✅ Setup complete!"
echo ""
echo "Next steps:"
echo "  1. Update .env if using different database credentials"
echo "  2. Run 'cargo build' to compile the project"
echo "  3. Run 'cargo run' to start the development server"
echo "  4. Visit http://localhost:3000/health to verify the app is running"
echo ""
echo "Documentation:"
echo "  - Setup guide: docs/setup.md"
echo "  - Architecture: docs/architecture.md"
echo "  - Partitioning: docs/database-partitioning.md"
