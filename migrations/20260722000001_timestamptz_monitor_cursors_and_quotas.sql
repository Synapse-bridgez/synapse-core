-- account_monitor_cursors.updated_at and api_quotas.created_at/updated_at were
-- declared as timezone-naive TIMESTAMP, inconsistent with every other
-- timestamp column in the schema (TIMESTAMPTZ). NOW() is TIMESTAMPTZ and gets
-- implicitly cast to the session's TimeZone on write; since all deployments
-- run with TimeZone=UTC, the stored wall-clock values are already UTC, so
-- this is a type/label fix rather than a data shift.
ALTER TABLE account_monitor_cursors
    ALTER updated_at TYPE TIMESTAMPTZ USING updated_at AT TIME ZONE 'UTC';

ALTER TABLE api_quotas
    ALTER created_at TYPE TIMESTAMPTZ USING created_at AT TIME ZONE 'UTC',
    ALTER updated_at TYPE TIMESTAMPTZ USING updated_at AT TIME ZONE 'UTC';
