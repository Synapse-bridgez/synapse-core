-- Also fixes (aside, unrelated to Parts A-E's literal scope but required to
-- make the existing test suite build/pass): the previous seed migration
-- (20260422000000) only precreated partitions through 2026-07. Running the
-- test suite today (2026-08-23) already exposed a dozen pre-existing test
-- failures (tests/search_test.rs) that raw-insert into `transactions` with
-- created_at = NOW() and hit 23514, because nothing seeded 2026-08 onward.
-- Part A's self-heal (ensure_partition_for, see 20260823000000) covers this
-- for any insert going through queries::insert_transaction, but plenty of
-- test fixtures (and any other raw-SQL insert path) don't go through that
-- function. This buys runway the same way the previous seed migration did,
-- rather than leaving the next calendar boundary to silently break tests
-- again in a future PR unrelated to partitioning.

CREATE TABLE IF NOT EXISTS transactions_y2026m08 PARTITION OF transactions FOR VALUES FROM ('2026-08-01') TO ('2026-09-01');
CREATE TABLE IF NOT EXISTS transactions_y2026m09 PARTITION OF transactions FOR VALUES FROM ('2026-09-01') TO ('2026-10-01');
CREATE TABLE IF NOT EXISTS transactions_y2026m10 PARTITION OF transactions FOR VALUES FROM ('2026-10-01') TO ('2026-11-01');
CREATE TABLE IF NOT EXISTS transactions_y2026m11 PARTITION OF transactions FOR VALUES FROM ('2026-11-01') TO ('2026-12-01');
CREATE TABLE IF NOT EXISTS transactions_y2026m12 PARTITION OF transactions FOR VALUES FROM ('2026-12-01') TO ('2027-01-01');
CREATE TABLE IF NOT EXISTS transactions_y2027m01 PARTITION OF transactions FOR VALUES FROM ('2027-01-01') TO ('2027-02-01');
CREATE TABLE IF NOT EXISTS transactions_y2027m02 PARTITION OF transactions FOR VALUES FROM ('2027-02-01') TO ('2027-03-01');
CREATE TABLE IF NOT EXISTS transactions_y2027m03 PARTITION OF transactions FOR VALUES FROM ('2027-03-01') TO ('2027-04-01');
CREATE TABLE IF NOT EXISTS transactions_y2027m04 PARTITION OF transactions FOR VALUES FROM ('2027-04-01') TO ('2027-05-01');
CREATE TABLE IF NOT EXISTS transactions_y2027m05 PARTITION OF transactions FOR VALUES FROM ('2027-05-01') TO ('2027-06-01');
CREATE TABLE IF NOT EXISTS transactions_y2027m06 PARTITION OF transactions FOR VALUES FROM ('2027-06-01') TO ('2027-07-01');
CREATE TABLE IF NOT EXISTS transactions_y2027m07 PARTITION OF transactions FOR VALUES FROM ('2027-07-01') TO ('2027-08-01');
CREATE TABLE IF NOT EXISTS transactions_y2027m08 PARTITION OF transactions FOR VALUES FROM ('2027-08-01') TO ('2027-09-01');
CREATE TABLE IF NOT EXISTS transactions_y2027m09 PARTITION OF transactions FOR VALUES FROM ('2027-09-01') TO ('2027-10-01');
CREATE TABLE IF NOT EXISTS transactions_y2027m10 PARTITION OF transactions FOR VALUES FROM ('2027-10-01') TO ('2027-11-01');
CREATE TABLE IF NOT EXISTS transactions_y2027m11 PARTITION OF transactions FOR VALUES FROM ('2027-11-01') TO ('2027-12-01');
CREATE TABLE IF NOT EXISTS transactions_y2027m12 PARTITION OF transactions FOR VALUES FROM ('2027-12-01') TO ('2028-01-01');
