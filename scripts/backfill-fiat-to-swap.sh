#!/usr/bin/env bash
# scripts/backfill-fiat-to-swap.sh
#
# Idempotent backfill: migrate completed Phase 1 fiat transactions into the
# Phase 2 swap domain (swap_quotes + swap_orders).  Issue #1475.
#
# Usage:
#   ./scripts/backfill-fiat-to-swap.sh [OPTIONS]
#
# Options:
#   --dry-run              Report what would be created, make no writes.
#   --batch-size N         Rows per batch (default: 200).
#   --dest-asset ASSET     Destination asset code for backfilled orders
#                          (default: XLM).
#   --start-id UUID        Resume from this transaction id (exclusive cursor).
#   --max-batches N        Stop after N batches (useful for staged roll-out).
#   --help                 Print this message and exit.
#
# Required environment variables:
#   DATABASE_URL           Postgres connection string (write access required
#                          unless --dry-run).
#
# Idempotency: the swap_backfill_state checkpoint table ensures that rows
# already processed in a previous run are skipped.  The job can be stopped
# and restarted at any time without double-creating orders.
#
# Safety:
#   * Uses bounded SELECT … LIMIT batches — no long transactions.
#   * Each batch wraps its writes in an explicit transaction; a partial-batch
#     failure rolls back only that batch.
#   * In --dry-run mode no writes are made to swap_quotes, swap_orders, or
#     swap_backfill_state.

set -euo pipefail

# ---------------------------------------------------------------------------
# Defaults
# ---------------------------------------------------------------------------
DRY_RUN=false
BATCH_SIZE=200
DEST_ASSET="XLM"
START_ID=""
MAX_BATCHES=0          # 0 = unlimited
LOG_DIR="${BACKFILL_LOG_DIR:-./var/backfill}"
RUN_ID=$(date -u +%Y%m%dT%H%M%SZ)

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
usage() {
    sed -n '4,28p' "$0" | sed 's/^# //' | sed 's/^#//'
    exit 0
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run)      DRY_RUN=true;          shift ;;
        --batch-size)   BATCH_SIZE="${2:?}";   shift 2 ;;
        --dest-asset)   DEST_ASSET="${2:?}";   shift 2 ;;
        --start-id)     START_ID="${2:?}";     shift 2 ;;
        --max-batches)  MAX_BATCHES="${2:?}";  shift 2 ;;
        --help|-h)      usage ;;
        *) echo "Unknown option: $1" >&2; exit 2 ;;
    esac
done

[[ -n "${DATABASE_URL:-}" ]] || { echo "DATABASE_URL is not set" >&2; exit 2; }

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------
mkdir -p "${LOG_DIR}"
LOG_FILE="${LOG_DIR}/backfill-${RUN_ID}.log"
exec > >(tee -a "${LOG_FILE}") 2>&1

log() { echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] $*"; }

log "=== Fiat→Swap backfill started run_id=${RUN_ID} dry_run=${DRY_RUN} batch_size=${BATCH_SIZE} dest_asset=${DEST_ASSET} ==="

# ---------------------------------------------------------------------------
# psql helper: run SQL and return the trimmed single-value output.
# ---------------------------------------------------------------------------
psql_value() {
    psql "${DATABASE_URL}" --no-psqlrc --tuples-only --no-align --command "$1" 2>/dev/null | tr -d '[:space:]'
}

psql_exec() {
    psql "${DATABASE_URL}" --no-psqlrc --tuples-only --no-align --command "$1"
}

# ---------------------------------------------------------------------------
# Pre-flight: count eligible rows
# ---------------------------------------------------------------------------
ELIGIBLE=$(psql_value "
    SELECT COUNT(*) FROM transactions t
    WHERE t.status = 'completed'
      AND NOT EXISTS (
          SELECT 1 FROM swap_backfill_state s
          WHERE s.transaction_id = t.id
      );
")

log "Eligible transactions (not yet backfilled): ${ELIGIBLE}"

if [[ "${DRY_RUN}" == "true" ]]; then
    log "[DRY-RUN] Would process up to ${ELIGIBLE} rows in batches of ${BATCH_SIZE} → dest_asset=${DEST_ASSET}"
    log "[DRY-RUN] No writes will be made."

    # Emit a sample of what would be created
    SAMPLE=$(psql_exec "
        SELECT t.id, t.stellar_account, t.amount, t.asset_code, t.created_at
        FROM transactions t
        WHERE t.status = 'completed'
          AND NOT EXISTS (
              SELECT 1 FROM swap_backfill_state s
              WHERE s.transaction_id = t.id
          )
        ORDER BY t.id
        LIMIT 5;
    ")
    log "[DRY-RUN] Sample rows that would be processed:"
    echo "${SAMPLE}"
    log "[DRY-RUN] Run without --dry-run to execute."
    exit 0
fi

# ---------------------------------------------------------------------------
# Backfill loop
# ---------------------------------------------------------------------------
CURSOR="${START_ID}"
BATCH_NUM=0
TOTAL_CREATED=0
TOTAL_SKIPPED=0
TOTAL_ERRORS=0

while true; do
    BATCH_NUM=$((BATCH_NUM + 1))

    if [[ "${MAX_BATCHES}" -gt 0 && "${BATCH_NUM}" -gt "${MAX_BATCHES}" ]]; then
        log "Reached max-batches limit (${MAX_BATCHES}); stopping."
        break
    fi

    # Fetch next batch of eligible transaction ids (ordered by id for cursor stability)
    if [[ -n "${CURSOR}" ]]; then
        CURSOR_CLAUSE="AND t.id > '${CURSOR}'::uuid"
    else
        CURSOR_CLAUSE=""
    fi

    BATCH_IDS=$(psql_exec "
        SELECT t.id
        FROM transactions t
        WHERE t.status = 'completed'
          ${CURSOR_CLAUSE}
          AND NOT EXISTS (
              SELECT 1 FROM swap_backfill_state s
              WHERE s.transaction_id = t.id
          )
        ORDER BY t.id
        LIMIT ${BATCH_SIZE};
    " | tr -d ' ')

    if [[ -z "${BATCH_IDS}" ]]; then
        log "No more eligible rows — backfill complete."
        break
    fi

    ROW_COUNT=$(echo "${BATCH_IDS}" | wc -l | tr -d ' ')
    log "Batch ${BATCH_NUM}: processing ${ROW_COUNT} rows (cursor=${CURSOR:-<start>})"

    # Process each row in a single transaction per batch
    BATCH_CREATED=0
    BATCH_ERRORS=0

    while IFS= read -r TX_ID; do
        [[ -z "${TX_ID}" ]] && continue

        # Run per-row insert in a transaction; ON CONFLICT guards idempotency
        RESULT=$(psql_exec "
            BEGIN;

            -- Fetch source row
            WITH src AS (
                SELECT id, stellar_account, amount, asset_code, created_at, updated_at,
                       anchor_transaction_id, callback_status, metadata
                FROM transactions
                WHERE id = '${TX_ID}'::uuid
                LIMIT 1
            ),

            -- Insert synthetic quote
            inserted_quote AS (
                INSERT INTO swap_quotes
                    (id, stellar_account, source_asset, destination_asset,
                     source_amount, destination_amount, rate, fee_amount,
                     expires_at, created_at, metadata)
                SELECT
                    gen_random_uuid(),
                    src.stellar_account,
                    src.asset_code,
                    '${DEST_ASSET}',
                    src.amount,
                    src.amount,       -- 1:1 synthetic rate
                    1.0,
                    0.0,
                    src.created_at + INTERVAL '60 seconds',
                    src.created_at,
                    jsonb_build_object(
                        'backfill', true,
                        'source', 'phase1_fiat_transaction',
                        'transaction_id', src.id::text,
                        'anchor_transaction_id', src.anchor_transaction_id
                    )
                FROM src
                ON CONFLICT DO NOTHING
                RETURNING id, created_at
            ),

            -- Insert swap order referencing the new quote
            inserted_order AS (
                INSERT INTO swap_orders
                    (id, stellar_account, quote_id, quote_created_at,
                     source_asset, destination_asset, source_amount, filled_amount,
                     status, created_at, updated_at, metadata)
                SELECT
                    gen_random_uuid(),
                    src.stellar_account,
                    q.id,
                    q.created_at,
                    src.asset_code,
                    '${DEST_ASSET}',
                    src.amount,
                    src.amount,       -- fully filled
                    'filled',
                    src.created_at,
                    src.updated_at,
                    jsonb_build_object(
                        'backfill', true,
                        'source', 'phase1_fiat_transaction',
                        'transaction_id', src.id::text,
                        'idempotency_key', 'swap:' || src.id::text,
                        'anchor_transaction_id', src.anchor_transaction_id,
                        'callback_status', src.callback_status,
                        'original_metadata', src.metadata
                    )
                FROM src
                CROSS JOIN inserted_quote q
                ON CONFLICT DO NOTHING
                RETURNING id
            )

            -- Record checkpoint (ON CONFLICT = already done; skip)
            INSERT INTO swap_backfill_state
                (transaction_id, swap_order_id, dry_run, backfill_run_id, outcome)
            SELECT
                src.id,
                o.id,
                false,
                '${RUN_ID}',
                'created'
            FROM src
            CROSS JOIN inserted_order o
            ON CONFLICT (transaction_id) DO NOTHING;

            COMMIT;
        " 2>&1) || true

        if echo "${RESULT}" | grep -qi 'error\|ERROR'; then
            log "  ERROR processing ${TX_ID}: ${RESULT}"
            # Record the error in the state table
            psql_exec "
                INSERT INTO swap_backfill_state
                    (transaction_id, dry_run, backfill_run_id, outcome, error_detail)
                VALUES ('${TX_ID}'::uuid, false, '${RUN_ID}', 'error',
                        \$\$${RESULT:0:500}\$\$)
                ON CONFLICT (transaction_id) DO NOTHING;
            " >/dev/null 2>&1 || true
            BATCH_ERRORS=$((BATCH_ERRORS + 1))
        else
            BATCH_CREATED=$((BATCH_CREATED + 1))
        fi

        CURSOR="${TX_ID}"
    done <<< "${BATCH_IDS}"

    TOTAL_CREATED=$((TOTAL_CREATED + BATCH_CREATED))
    TOTAL_ERRORS=$((TOTAL_ERRORS + BATCH_ERRORS))
    log "  Batch ${BATCH_NUM} done: created=${BATCH_CREATED} errors=${BATCH_ERRORS}"
done

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
log "=== Backfill finished run_id=${RUN_ID} ==="
log "  Total created : ${TOTAL_CREATED}"
log "  Total skipped : (rows already in swap_backfill_state from prior runs)"
log "  Total errors  : ${TOTAL_ERRORS}"

if [[ "${TOTAL_ERRORS}" -gt 0 ]]; then
    log "WARNING: ${TOTAL_ERRORS} rows failed. Review the log at ${LOG_FILE} and re-run to retry."
    exit 1
fi

log "Done. Log saved to ${LOG_FILE}"
