-- #1508: time-partitioned (monthly) archive of every issued quote (durable,
-- full fidelity) and sampled normalized price snapshots.
CREATE TABLE IF NOT EXISTS swap_quote_archive (
    id          UUID        NOT NULL DEFAULT gen_random_uuid(),
    quote_id    UUID        NOT NULL,
    order_id    UUID,
    tenant_id   UUID        NOT NULL,
    pair        TEXT        NOT NULL,
    inputs      JSONB       NOT NULL,
    route       JSONB       NOT NULL,
    output      JSONB       NOT NULL,
    quoted_at   TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (id, quoted_at)
) PARTITION BY RANGE (quoted_at);
CREATE INDEX IF NOT EXISTS idx_swap_quote_archive_quote ON swap_quote_archive (quote_id);
CREATE INDEX IF NOT EXISTS idx_swap_quote_archive_order ON swap_quote_archive (order_id) WHERE order_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_swap_quote_archive_tenant ON swap_quote_archive (tenant_id, pair, quoted_at);

CREATE TABLE IF NOT EXISTS swap_price_snapshots (
    id          UUID        NOT NULL DEFAULT gen_random_uuid(),
    pair        TEXT        NOT NULL,
    source      TEXT        NOT NULL DEFAULT 'aggregate',
    bid         NUMERIC     NOT NULL,
    ask         NUMERIC     NOT NULL,
    mid         NUMERIC     NOT NULL,
    captured_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (id, captured_at)
) PARTITION BY RANGE (captured_at);
CREATE INDEX IF NOT EXISTS idx_swap_price_snapshots_pair ON swap_price_snapshots (pair, captured_at);

-- Detached partitions awaiting / completed export to object storage.
CREATE TABLE IF NOT EXISTS swap_archive_exports (
    partition_name TEXT        PRIMARY KEY,
    status         TEXT        NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'exported', 'failed')),
    detached_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    exported_at    TIMESTAMPTZ,
    last_error     TEXT
);

-- Creates monthly partitions for the current month and `months_ahead` more.
CREATE OR REPLACE FUNCTION ensure_swap_archive_partitions(months_ahead INT DEFAULT 3)
RETURNS INT AS $$
DECLARE
    m INT; start_ts TIMESTAMPTZ; end_ts TIMESTAMPTZ; tbl TEXT; created INT := 0; part TEXT;
BEGIN
    FOREACH tbl IN ARRAY ARRAY['swap_quote_archive', 'swap_price_snapshots'] LOOP
        FOR m IN 0..months_ahead LOOP
            start_ts := date_trunc('month', now() AT TIME ZONE 'UTC') + make_interval(months => m);
            end_ts := start_ts + interval '1 month';
            part := tbl || '_' || to_char(start_ts, 'YYYY_MM');
            IF to_regclass(part) IS NULL THEN
                EXECUTE format('CREATE TABLE %I PARTITION OF %I FOR VALUES FROM (%L) TO (%L)',
                               part, tbl, start_ts, end_ts);
                created := created + 1;
            END IF;
        END LOOP;
    END LOOP;
    RETURN created;
END;
$$ LANGUAGE plpgsql;

-- Detaches partitions whose whole range is older than the retention cutoff and
-- queues them for export; the detached table is dropped only after export.
CREATE OR REPLACE FUNCTION detach_swap_archive_partitions(retention_months INT)
RETURNS INT AS $$
DECLARE
    r RECORD; cutoff TIMESTAMPTZ; detached INT := 0; part_month TIMESTAMPTZ;
BEGIN
    cutoff := date_trunc('month', now() AT TIME ZONE 'UTC') - make_interval(months => retention_months);
    FOR r IN
        SELECT c.relname AS part, p.relname AS parent
        FROM pg_inherits i
        JOIN pg_class c ON c.oid = i.inhrelid
        JOIN pg_class p ON p.oid = i.inhparent
        WHERE p.relname IN ('swap_quote_archive', 'swap_price_snapshots')
    LOOP
        part_month := to_timestamp(right(r.part, 7), 'YYYY_MM') AT TIME ZONE 'UTC';
        IF part_month < cutoff THEN
            EXECUTE format('ALTER TABLE %I DETACH PARTITION %I', r.parent, r.part);
            INSERT INTO swap_archive_exports (partition_name) VALUES (r.part) ON CONFLICT DO NOTHING;
            detached := detached + 1;
        END IF;
    END LOOP;
    RETURN detached;
END;
$$ LANGUAGE plpgsql;

SELECT ensure_swap_archive_partitions(3);
