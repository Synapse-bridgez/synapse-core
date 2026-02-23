#!/bin/bash

# Load environment variables
if [ -f .env ]; then
    export $(cat .env | grep -v '^#' | xargs)
fi

# Check if DATABASE_URL is set
if [ -z "$DATABASE_URL" ]; then
    echo "Error: DATABASE_URL not set"
    echo "Please create a .env file with DATABASE_URL=postgresql://user:password@localhost:5432/synapse_core"
    exit 1
fi

echo "Running migrations..."

# Run migrations in order
for migration in migrations/*.sql; do
    echo "Applying $migration..."
    psql "$DATABASE_URL" -f "$migration"
    if [ $? -ne 0 ]; then
        echo "Error applying $migration"
        exit 1
    fi
done

echo "All migrations completed successfully!"
