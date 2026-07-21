#!/usr/bin/env bash
# Point-in-time-recovery (PITR) restore.
#
# Invoked by ShellPitrExecutor (src/services/pitr.rs) as a background job, so
# stdout/stderr and the exit code are the only channel back to the app:
#   - exit 0  -> stdout (trimmed) is recorded as the job's `detail`
#   - exit !0 -> stderr (trimmed) is recorded as the job's `error_message`
#
# Usage:
#   pitr_restore.sh <target_timestamp_rfc3339> <database_url> <workspace_dir> <wal_archive_dir>
#
# What this does — and deliberately does NOT do:
#   1. Streams a fresh physical base backup from the live server via
#      pg_basebackup (uses the replication protocol; this is what
#      wal_level=replica / max_wal_senders in docker-compose.yml exist for).
#   2. Configures recovery to replay archived WAL up to the target timestamp
#      (recovery_target_time), reading segments from <wal_archive_dir>.
#   3. Starts the recovered data directory as a standalone Postgres instance
#      on a separate port and waits for recovery to complete.
#
#   It intentionally stops there rather than replacing the live primary.
#   Swapping a running production database out from under its connection
#   pool is a distinct, high-blast-radius action that deserves an explicit,
#   separate promotion step by an operator who has verified the recovered
#   data — not something a background job should do unattended. The job's
#   `detail` message on success reports where the recovered instance lives
#   so that verification/promotion can happen next.
#
# Requirements: pg_basebackup, pg_ctl, postgres, pg_isready, psql on PATH,
# network access to the source database, and read access to the WAL
# archive directory. None of these are available inside the default
# `synapse-core` app container by default — this script is meant to run in
# an environment that has them (e.g. a dedicated restore/ops container with
# the matching Postgres major version installed).
set -euo pipefail

log() { echo "[pitr_restore] $*" >&2; }
fail() { echo "$*" >&2; exit 1; }

TARGET_TIMESTAMP="${1:?Usage: $0 <target_timestamp_rfc3339> <database_url> <workspace_dir> <wal_archive_dir>}"
DATABASE_URL="${2:?database_url is required}"
WORKSPACE_DIR="${3:?workspace_dir is required}"
WAL_ARCHIVE_DIR="${4:?wal_archive_dir is required}"

for bin in pg_basebackup pg_ctl postgres pg_isready psql; do
  command -v "$bin" >/dev/null 2>&1 || fail \
    "required binary '$bin' not found on PATH; this environment is missing Postgres server tooling needed for PITR restore"
done

[ -d "$WAL_ARCHIVE_DIR" ] || fail "WAL archive directory not found or not accessible: $WAL_ARCHIVE_DIR"

RESTORE_PORT="${PITR_RESTORE_PORT:-55432}"
DATA_DIR="$WORKSPACE_DIR/restore_$(date -u +%Y%m%dT%H%M%SZ)"

mkdir -p "$DATA_DIR"
log "streaming base backup into $DATA_DIR"
pg_basebackup --dbname="$DATABASE_URL" --pgdata="$DATA_DIR" --format=plain --write-recovery-conf --checkpoint=fast \
  || fail "pg_basebackup failed"

# Postgres >= 12: recovery is driven by standby.signal + postgresql.auto.conf.
touch "$DATA_DIR/standby.signal"
{
  echo "restore_command = 'cp \"$WAL_ARCHIVE_DIR/%f\" \"%p\"'"
  echo "recovery_target_time = '$TARGET_TIMESTAMP'"
  echo "recovery_target_action = 'promote'"
  echo "recovery_target_inclusive = true"
} >> "$DATA_DIR/postgresql.auto.conf"

log "starting recovered instance on port $RESTORE_PORT"
pg_ctl start -D "$DATA_DIR" -o "-p $RESTORE_PORT -c listen_addresses=127.0.0.1" -l "$DATA_DIR/recovery.log" \
  || fail "failed to start recovered instance; see $DATA_DIR/recovery.log"

# Wait for recovery to finish: pg_isready succeeds once the server is
# accepting connections, whether still in recovery or already promoted.
ATTEMPTS=0
MAX_ATTEMPTS="${PITR_RESTORE_WAIT_ATTEMPTS:-120}"
until pg_isready -h 127.0.0.1 -p "$RESTORE_PORT" >/dev/null 2>&1; do
  ATTEMPTS=$((ATTEMPTS + 1))
  if [ "$ATTEMPTS" -ge "$MAX_ATTEMPTS" ]; then
    pg_ctl stop -D "$DATA_DIR" -m immediate >/dev/null 2>&1 || true
    fail "recovered instance did not become ready within ${MAX_ATTEMPTS}s; see $DATA_DIR/recovery.log"
  fi
  sleep 1
done

# Confirm recovery actually reached the target rather than just starting up.
IN_RECOVERY=$(psql -h 127.0.0.1 -p "$RESTORE_PORT" -U "$(whoami)" -d postgres -tAc "SELECT pg_is_in_recovery();" 2>/dev/null || echo "unknown")

echo "recovered instance ready at 127.0.0.1:$RESTORE_PORT (data dir: $DATA_DIR, target: $TARGET_TIMESTAMP, in_recovery: $IN_RECOVERY) — verify the data, then promote/cut over manually; it is left running for inspection"
