-- Phase 2 swap domain persistence: quotes, orders, fills, and an append-only
-- lifecycle event log. Follows the monthly range-partitioning pattern used by
-- `transactions` (see docs/partitioning.md and
-- 20250217000000_partition_transactions.sql).
--
-- Partitioning notes:
-- - Every table is PARTITION BY RANGE (created_at) with PRIMARY KEY (id, created_at),
--   because PostgreSQL requires the partition key in any unique constraint.
-- - Because of that, foreign keys to a partitioned parent carry the parent's
--   created_at alongside the id (quote_id + quote_created_at, order_id + order_created_at).
--   Referencing (id, created_at) also keeps the FK on the partitioned PK index.
--
-- Monetary amounts are NUMERIC (exact decimal), never floating point.
--
-- Status domains (CHECK-constrained; transition rules are enforced by the
-- swap engine, not by the database):
--   swap_orders.status / swap_events.{from_status,to_status}:
--     pending, routed, partially_filled, filled, failed, cancelled

-- Quotes are immutable once issued; expiry is derived from expires_at.
CREATE TABLE swap_quotes (
    id UUID DEFAULT gen_random_uuid(),
    tenant_id UUID,
    stellar_account VARCHAR(56) NOT NULL,
    source_asset VARCHAR(12) NOT NULL,
    destination_asset VARCHAR(12) NOT NULL,
    source_amount NUMERIC NOT NULL CHECK (source_amount > 0),
    destination_amount NUMERIC NOT NULL CHECK (destination_amount > 0),
    rate NUMERIC NOT NULL CHECK (rate > 0),
    fee_amount NUMERIC NOT NULL DEFAULT 0 CHECK (fee_amount >= 0),
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    metadata JSONB,
    PRIMARY KEY (id, created_at),
    CHECK (expires_at > created_at)
) PARTITION BY RANGE (created_at);

CREATE INDEX idx_swap_quotes_tenant_account ON swap_quotes (tenant_id, stellar_account, created_at);

-- Orders reference the quote they were executed against.
CREATE TABLE swap_orders (
    id UUID DEFAULT gen_random_uuid(),
    tenant_id UUID,
    stellar_account VARCHAR(56) NOT NULL,
    quote_id UUID NOT NULL,
    quote_created_at TIMESTAMPTZ NOT NULL,
    source_asset VARCHAR(12) NOT NULL,
    destination_asset VARCHAR(12) NOT NULL,
    source_amount NUMERIC NOT NULL CHECK (source_amount > 0),
    filled_amount NUMERIC NOT NULL DEFAULT 0 CHECK (filled_amount >= 0),
    status VARCHAR(20) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'routed', 'partially_filled', 'filled', 'failed', 'cancelled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    metadata JSONB,
    PRIMARY KEY (id, created_at),
    FOREIGN KEY (quote_id, quote_created_at) REFERENCES swap_quotes (id, created_at)
) PARTITION BY RANGE (created_at);

CREATE INDEX idx_swap_orders_tenant_account_status ON swap_orders (tenant_id, stellar_account, status, created_at);
CREATE INDEX idx_swap_orders_status ON swap_orders (status, created_at);
CREATE INDEX idx_swap_orders_quote ON swap_orders (quote_id, quote_created_at);

-- Fills are immutable execution records against an order.
CREATE TABLE swap_fills (
    id UUID DEFAULT gen_random_uuid(),
    order_id UUID NOT NULL,
    order_created_at TIMESTAMPTZ NOT NULL,
    source_amount NUMERIC NOT NULL CHECK (source_amount > 0),
    destination_amount NUMERIC NOT NULL CHECK (destination_amount > 0),
    rate NUMERIC NOT NULL CHECK (rate > 0),
    fee_amount NUMERIC NOT NULL DEFAULT 0 CHECK (fee_amount >= 0),
    external_ref VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (id, created_at),
    FOREIGN KEY (order_id, order_created_at) REFERENCES swap_orders (id, created_at)
) PARTITION BY RANGE (created_at);

CREATE INDEX idx_swap_fills_order ON swap_fills (order_id, created_at);

-- Append-only lifecycle log of order transitions.
CREATE TABLE swap_events (
    id UUID DEFAULT gen_random_uuid(),
    order_id UUID NOT NULL,
    order_created_at TIMESTAMPTZ NOT NULL,
    from_status VARCHAR(20)
        CHECK (from_status IN ('pending', 'routed', 'partially_filled', 'filled', 'failed', 'cancelled')),
    to_status VARCHAR(20) NOT NULL
        CHECK (to_status IN ('pending', 'routed', 'partially_filled', 'filled', 'failed', 'cancelled')),
    payload JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (id, created_at),
    FOREIGN KEY (order_id, order_created_at) REFERENCES swap_orders (id, created_at)
) PARTITION BY RANGE (created_at);

CREATE INDEX idx_swap_events_order ON swap_events (order_id, created_at);

-- Enforce append-only at the database layer: any UPDATE or DELETE on the event
-- log (including through a partition) is rejected. Row triggers on a
-- partitioned parent are cloned onto partitions created later.
CREATE OR REPLACE FUNCTION swap_events_reject_mutation()
RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'swap_events is append-only: % is not permitted', TG_OP
        USING ERRCODE = 'insufficient_privilege';
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER swap_events_append_only
    BEFORE UPDATE OR DELETE ON swap_events
    FOR EACH ROW EXECUTE FUNCTION swap_events_reject_mutation();

-- Partition maintenance for the swap tables, built on the generalized routines
-- from 20260928000000_generalize_partition_functions.sql.

CREATE OR REPLACE FUNCTION ensure_swap_partitions(months_ahead INT)
RETURNS void AS $$
DECLARE
    tbl TEXT;
    i INT;
BEGIN
    FOREACH tbl IN ARRAY ARRAY['swap_quotes', 'swap_orders', 'swap_fills', 'swap_events'] LOOP
        FOR i IN 0..(months_ahead - 1) LOOP
            PERFORM ensure_partition_for_table(tbl, (DATE_TRUNC('month', NOW()) + make_interval(months => i))::DATE);
        END LOOP;
    END LOOP;
END;
$$ LANGUAGE plpgsql;

-- Retention: swap records are audit data, so they are kept longer than the
-- 12-month `transactions` default. Detached partitions remain regular tables
-- and can be archived per docs/partitioning.md.
CREATE OR REPLACE FUNCTION detach_old_swap_partitions(retention_months INTEGER DEFAULT 24)
RETURNS void AS $$
DECLARE
    tbl TEXT;
BEGIN
    FOREACH tbl IN ARRAY ARRAY['swap_quotes', 'swap_orders', 'swap_fills', 'swap_events'] LOOP
        PERFORM detach_old_partitions_for_table(tbl, retention_months);
    END LOOP;
END;
$$ LANGUAGE plpgsql;

-- Create current month + 5 months of partitions for the swap tables.
SELECT ensure_swap_partitions(6);
