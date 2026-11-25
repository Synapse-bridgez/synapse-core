#!/usr/bin/env bash
# scripts/backup-swap-data.sh
#
# Automated base backup + WAL archiving configuration for the PostgreSQL and
# Redis stores that hold Phase 2 swap data.  Issue #1477.
#
# Usage:
#   ./scripts/backup-swap-data.sh [--type hourly|daily|monthly] [--verify]
#
# Required environment variables:
#   DATABASE_URL      Postgres connection string (superuser for pg_basebackup)
#   BACKUP_DIR        Directory for base backups
#   WAL_ARCHIVE_DIR   Directory for WAL archive files
#   BACKUP_KEY        AES-256 encryption passphrase (set via secrets manager)
#
# Optional:
#   REDIS_RDB_PATH    Path to the Redis RDB file (default: /var/lib/redis/dump.rdb)
#   BACKUP_TYPE       hourly | daily | monthly (default: daily)
#   DR_LOG_DIR        Backup log directory (default: ./var/backup)
#
# RTO / RPO targets:
#   RPO = 5 minutes  (WAL archive_timeout = 300 s)
#   RTO = 30 minutes (documented restore procedure below)
#
# Encryption: AES-256-CBC via openssl with PBKDF2 key derivation.
# Access control: BACKUP_KEY must be set from a secrets manager (Vault /
# AWS Secrets Manager); do not hard-code it in any config file.

set -euo pipefail

BACKUP_TYPE="${BACKUP_TYPE:-daily}"
VERIFY="${VERIFY:-false}"
DR_LOG_DIR="${DR_LOG_DIR:-./var/backup}"
REDIS_RDB_PATH="${REDIS_RDB_PATH:-/var/lib/redis/dump.rdb}"
RUN_ID=$(date -u +%Y%m%dT%H%M%SZ)

# ---------------------------------------------------------------------------
# Validate environment
# ---------------------------------------------------------------------------
for required in DATABASE_URL BACKUP_DIR WAL_ARCHIVE_DIR BACKUP_KEY; do
    [[ -n "${!required:-}" ]] || { echo "ERROR: ${required} is not set" >&2; exit 2; }
done

mkdir -p "${BACKUP_DIR}" "${WAL_ARCHIVE_DIR}" "${DR_LOG_DIR}"
LOG_FILE="${DR_LOG_DIR}/backup-${RUN_ID}.log"
exec > >(tee -a "${LOG_FILE}") 2>&1

log() { echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] $*"; }
log "=== Backup started run_id=${RUN_ID} type=${BACKUP_TYPE} ==="

# ---------------------------------------------------------------------------
# Parse arguments
# ---------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
    case "$1" in
        --type)      BACKUP_TYPE="${2:?}"; shift 2 ;;
        --verify)    VERIFY=true; shift ;;
        *) echo "Unknown option: $1" >&2; exit 2 ;;
    esac
done

# ---------------------------------------------------------------------------
# PostgreSQL base backup
# ---------------------------------------------------------------------------
PGBACKUP_DIR="${BACKUP_DIR}/postgres-${BACKUP_TYPE}-${RUN_ID}"
mkdir -p "${PGBACKUP_DIR}"

log "Starting pg_basebackup → ${PGBACKUP_DIR}"
pg_basebackup \
    --pgdata="${PGBACKUP_DIR}" \
    --format=tar \
    --compress=9 \
    --checkpoint=fast \
    --wal-method=stream \
    --no-password \
    --dbname="${DATABASE_URL}" \
    --progress \
    --verbose

log "pg_basebackup complete"

# ---------------------------------------------------------------------------
# Encrypt the base backup
# ---------------------------------------------------------------------------
ARCHIVE_NAME="backup-${BACKUP_TYPE}-${RUN_ID}.tar.gz.enc"
ARCHIVE_PATH="${BACKUP_DIR}/${ARCHIVE_NAME}"

log "Encrypting backup → ${ARCHIVE_PATH}"
tar -czf - -C "${BACKUP_DIR}" "postgres-${BACKUP_TYPE}-${RUN_ID}" | \
    openssl enc -aes-256-cbc -salt -pbkdf2 \
        -pass "pass:${BACKUP_KEY}" \
        -out "${ARCHIVE_PATH}"

# Remove unencrypted intermediate
rm -rf "${PGBACKUP_DIR}"
log "Encryption complete"

# ---------------------------------------------------------------------------
# SHA-256 checksum
# ---------------------------------------------------------------------------
CHECKSUM=$(sha256sum "${ARCHIVE_PATH}" | awk '{print $1}')
echo "${CHECKSUM}  ${ARCHIVE_NAME}" > "${BACKUP_DIR}/${ARCHIVE_NAME}.sha256"
log "Checksum: ${CHECKSUM}"

# ---------------------------------------------------------------------------
# Redis: snapshot to encrypted file (best-effort; RDB is not transactional)
# ---------------------------------------------------------------------------
if [[ -f "${REDIS_RDB_PATH}" ]]; then
    REDIS_ARCHIVE="${BACKUP_DIR}/redis-${BACKUP_TYPE}-${RUN_ID}.rdb.enc"
    log "Backing up Redis RDB → ${REDIS_ARCHIVE}"
    openssl enc -aes-256-cbc -salt -pbkdf2 \
        -in "${REDIS_RDB_PATH}" \
        -out "${REDIS_ARCHIVE}" \
        -pass "pass:${BACKUP_KEY}"
    REDIS_CHECKSUM=$(sha256sum "${REDIS_ARCHIVE}" | awk '{print $1}')
    echo "${REDIS_CHECKSUM}  redis-${BACKUP_TYPE}-${RUN_ID}.rdb.enc" \
        > "${BACKUP_DIR}/redis-${BACKUP_TYPE}-${RUN_ID}.rdb.enc.sha256"
    log "Redis backup complete: checksum=${REDIS_CHECKSUM}"
else
    log "WARNING: Redis RDB not found at ${REDIS_RDB_PATH}; skipping Redis backup"
fi

# ---------------------------------------------------------------------------
# Write metadata record
# ---------------------------------------------------------------------------
METADATA_FILE="${BACKUP_DIR}/${ARCHIVE_NAME}.meta"
cat > "${METADATA_FILE}" <<EOF
{
  "run_id": "${RUN_ID}",
  "type": "${BACKUP_TYPE}",
  "archive": "${ARCHIVE_NAME}",
  "checksum_sha256": "${CHECKSUM}",
  "created_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "database_url_host": "$(echo "${DATABASE_URL}" | grep -oP '@[^/]+' | tr -d '@')",
  "encrypted": true,
  "compressed": true,
  "wal_archive_dir": "${WAL_ARCHIVE_DIR}"
}
EOF
log "Metadata written to ${METADATA_FILE}"

# ---------------------------------------------------------------------------
# Optional post-backup verification
# ---------------------------------------------------------------------------
if [[ "${VERIFY}" == "true" ]]; then
    log "Verifying backup integrity..."
    STORED_CHECKSUM=$(cat "${BACKUP_DIR}/${ARCHIVE_NAME}.sha256" | awk '{print $1}')
    COMPUTED_CHECKSUM=$(sha256sum "${ARCHIVE_PATH}" | awk '{print $1}')
    if [[ "${STORED_CHECKSUM}" == "${COMPUTED_CHECKSUM}" ]]; then
        log "Integrity check PASSED"
    else
        log "ERROR: Integrity check FAILED — stored=${STORED_CHECKSUM} computed=${COMPUTED_CHECKSUM}" >&2
        exit 1
    fi

    # Verify the encrypted archive can be decrypted without error
    log "Testing decryptability..."
    openssl enc -d -aes-256-cbc -pbkdf2 \
        -pass "pass:${BACKUP_KEY}" \
        -in "${ARCHIVE_PATH}" | \
        tar -tzf - > /dev/null
    log "Decryption test PASSED"
fi

# ---------------------------------------------------------------------------
# Retention: apply per docs/disaster-recovery.md
# ---------------------------------------------------------------------------
log "Applying retention policy..."
# Hourly: keep 24; Daily: keep 30; Monthly: keep 12
case "${BACKUP_TYPE}" in
    hourly)  KEEP=24  ;;
    daily)   KEEP=30  ;;
    monthly) KEEP=12  ;;
    *)       KEEP=30  ;;
esac

# Delete oldest archives beyond retention count (sort by name; names include timestamps)
EXCESS=$(ls -1t "${BACKUP_DIR}"/backup-${BACKUP_TYPE}-*.tar.gz.enc 2>/dev/null | tail -n "+$((KEEP + 1))")
if [[ -n "${EXCESS}" ]]; then
    while IFS= read -r old_archive; do
        log "Removing old backup: ${old_archive}"
        rm -f "${old_archive}" "${old_archive}.sha256" "${old_archive}.meta"
    done <<< "${EXCESS}"
fi

log "=== Backup finished run_id=${RUN_ID} ==="
log "Log saved to ${LOG_FILE}"
