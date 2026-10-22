-- Phase 2 swap tables, range-partitioned by created_at (#1447).
--
-- Follows the transactions partitioning pattern: monthly RANGE partitions on
-- created_at, named <parent>_yYYYYmMM, created ahead of time by
-- ensure_swap_partitions() and detached (not dropped) by detach_swap_partitions().
--
-- Uniqueness: a partitioned table's unique/PK constraints must include the
-- partition key, so swap_orders cannot enforce a global unique id on its own.
-- Idempotency keys therefore live in swap_order_idempotency, a non-partitioned
-- table that is the single authority for "this (tenant, key) already created an
-- order". Its rows are pruned by detach_swap_partitions() alongside the orders.

CREATE TABLE IF NOT EXISTS swap_orders (
    id             UUID        NOT NULL DEFAULT gen_random_uuid(),
    tenant_id      UUID        NOT NULL,
    transaction_id UUID,
    status         VARCHAR(20) NOT NULL DEFAULT 'pending'
                   CHECK (status IN ('pending', 'quoted', 'executing', 'completed', 'failed', 'cancelled')),
    source_asset   VARCHAR(12) NOT NULL,
    dest_asset     VARCHAR(12) NOT NULL,
    amount         NUMERIC     NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (id, created_at)
) PARTITION BY RANGE (created_at);

CREATE INDEX IF NOT EXISTS idx_swap_orders_tenant_created_at ON swap_orders (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_swap_orders_status ON swap_orders (status, created_at);

CREATE TABLE IF NOT EXISTS swap_quotes (
    id          UUID        NOT NULL DEFAULT gen_random_uuid(),
    order_id    UUID        NOT NULL,
    tenant_id   UUID        NOT NULL,
    price       NUMERIC     NOT NULL,
    fee         NUMERIC     NOT NULL DEFAULT 0,
    expires_at  TIMESTAMPTZ NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (id, created_at)
) PARTITION BY RANGE (created_at);

CREATE INDEX IF NOT EXISTS idx_swap_quotes_tenant_created_at ON swap_quotes (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_swap_quotes_order_id ON swap_quotes (order_id);

CREATE TABLE IF NOT EXISTS swap_fills (
    id          UUID        NOT NULL DEFAULT gen_random_uuid(),
    order_id    UUID        NOT NULL,
    quote_id    UUID,
    tenant_id   UUID        NOT NULL,
    amount      NUMERIC     NOT NULL,
    price       NUMERIC     NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (id, created_at)
) PARTITION BY RANGE (created_at);

CREATE INDEX IF NOT EXISTS idx_swap_fills_tenant_created_at ON swap_fills (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_swap_fills_order_id ON swap_fills (order_id);

-- Transactional outbox. Rows with published_at IS NULL are not yet delivered
-- and block detachment of their partition.
CREATE TABLE IF NOT EXISTS swap_events (
    id            UUID         NOT NULL DEFAULT gen_random_uuid(),
    aggregate_id  UUID         NOT NULL,
    tenant_id     UUID         NOT NULL,
    event_type    VARCHAR(64)  NOT NULL,
    payload       JSONB        NOT NULL DEFAULT '{}'::jsonb,
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    published_at  TIMESTAMPTZ,
    PRIMARY KEY (id, created_at)
) PARTITION BY RANGE (created_at);

CREATE INDEX IF NOT EXISTS idx_swap_events_tenant_created_at ON swap_events (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_swap_events_unpublished ON swap_events (created_at) WHERE published_at IS NULL;

CREATE TABLE IF NOT EXISTS swap_order_idempotency (
    tenant_id        UUID         NOT NULL,
    idempotency_key  VARCHAR(255) NOT NULL,
    order_id         UUID         NOT NULL,
    created_at       TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, idempotency_key)
);

CREATE INDEX IF NOT EXISTS idx_swap_order_idempotency_created_at ON swap_order_idempotency (created_at);

-- Creates the monthly partitions for one target month across all swap tables.
-- Same advisory-lock + duplicate_table pattern as ensure_partition_for().
CREATE OR REPLACE FUNCTION ensure_swap_partition_for(target_date DATE)
RETURNS void AS $$
DECLARE
    partition_date DATE;
    suffix TEXT;
    parent TEXT;
    start_date TEXT;
    end_date TEXT;
BEGIN
    partition_date := DATE_TRUNC('month', target_date);
    suffix := 'y' || TO_CHAR(partition_date, 'YYYY') || 'm' || TO_CHAR(partition_date, 'MM');
    start_date := TO_CHAR(partition_date, 'YYYY-MM-DD');
    end_date := TO_CHAR(partition_date + INTERVAL '1 month', 'YYYY-MM-DD');

    FOREACH parent IN ARRAY ARRAY['swap_orders', 'swap_quotes', 'swap_fills', 'swap_events'] LOOP
        PERFORM pg_advisory_xact_lock(hashtext(parent || '_' || suffix));

        BEGIN
            EXECUTE format(
                'CREATE TABLE IF NOT EXISTS %I PARTITION OF %I FOR VALUES FROM (%L) TO (%L)',
                parent || '_' || suffix, parent, start_date, end_date
            );
        EXCEPTION WHEN duplicate_table THEN
            -- A concurrent caller already created it; treat as success.
            NULL;
        END;
    END LOOP;
END;
$$ LANGUAGE plpgsql;

-- Ensures partitions exist for the current month and the next (months_ahead - 1).
-- Called by PartitionManager with the same lookahead as the transactions table.
CREATE OR REPLACE FUNCTION ensure_swap_partitions(months_ahead INT)
RETURNS void AS $$
DECLARE
    i INT;
BEGIN
    FOR i IN 0..(months_ahead - 1) LOOP
        PERFORM ensure_swap_partition_for((DATE_TRUNC('month', NOW()) + make_interval(months => i))::DATE);
    END LOOP;
END;
$$ LANGUAGE plpgsql;

-- Detaches swap partitions whose month starts before the retention cutoff.
--
-- A partition is kept (and logged) while any order it could belong to is still
-- non-terminal: a non-terminal order created before the partition's end blocks
-- detachment, because its quotes/fills may live in this partition or later ones.
-- For swap_events, unpublished outbox rows in the partition also block it.
-- Detached partitions remain as standalone tables for archival (see docs/partitioning.md).
CREATE OR REPLACE FUNCTION detach_swap_partitions(retention_months INT)
RETURNS void AS $$
DECLARE
    parent TEXT;
    child RECORD;
    part_start DATE;
    part_end DATE;
    cutoff DATE;
    blocked BOOLEAN;
BEGIN
    cutoff := DATE_TRUNC('month', NOW() - make_interval(months => retention_months))::DATE;

    FOREACH parent IN ARRAY ARRAY['swap_orders', 'swap_quotes', 'swap_fills', 'swap_events'] LOOP
        FOR child IN
            SELECT c.relname AS partition_name
            FROM pg_class c
            JOIN pg_inherits i ON c.oid = i.inhrelid
            JOIN pg_class p ON i.inhparent = p.oid
            WHERE p.relname = parent
              AND c.relname ~ '^swap_[a-z]+_y[0-9]{4}m[0-9]{2}$'
            ORDER BY c.relname
        LOOP
            part_start := make_date(
                SUBSTRING(child.partition_name FROM '_y([0-9]{4})m')::INT,
                SUBSTRING(child.partition_name FROM 'm([0-9]{2})$')::INT,
                1
            );
            IF part_start >= cutoff THEN
                CONTINUE;
            END IF;
            part_end := (part_start + INTERVAL '1 month')::DATE;

            blocked := EXISTS (
                SELECT 1 FROM swap_orders
                WHERE status NOT IN ('completed', 'failed', 'cancelled')
                  AND created_at < part_end
            );
            IF parent = 'swap_events' THEN
                blocked := blocked OR EXISTS (
                    SELECT 1 FROM swap_events
                    WHERE published_at IS NULL
                      AND created_at >= part_start
                      AND created_at < part_end
                );
            END IF;

            IF blocked THEN
                RAISE NOTICE 'Skipping detach of %: non-terminal orders or unpublished events remain', child.partition_name;
                CONTINUE;
            END IF;

            EXECUTE format('ALTER TABLE %I DETACH PARTITION %I', parent, child.partition_name);
            RAISE NOTICE 'Detached swap partition: %', child.partition_name;

            -- Idempotency keys share the orders retention window. Safe because the
            -- blocked check above guarantees no non-terminal order predates part_end.
            IF parent = 'swap_orders' THEN
                DELETE FROM swap_order_idempotency WHERE created_at < part_end;
            END IF;
        END LOOP;
    END LOOP;
END;
$$ LANGUAGE plpgsql;

-- Pre-create partitions for the current month and the two after it.
SELECT ensure_swap_partitions(3);
