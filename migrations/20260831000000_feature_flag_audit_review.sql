-- Governance review view over feature_flag_audit_logs (migrations/20260426000002_feature_flag_audit.sql).
--
-- Retrospective review tooling only (issue: feature flag audit governance review) — not an approval
-- gate. Lists who changed which flag, when, old/new value, and flags changes made outside normal
-- business hours (UTC 09:00-18:00 Mon-Fri) as a soft signal worth a closer look, not a hard block.
--
-- Note: feature_flag_audit_logs has no column for a linked justification (PR/ticket reference) as of
-- this migration, so this view cannot surface one. If/when such a column is added, extend this view
-- to include it rather than adding a second view.
CREATE OR REPLACE VIEW feature_flag_audit_review AS
SELECT
    id,
    flag_name,
    actor,
    old_value,
    new_value,
    "timestamp" AS changed_at,
    EXTRACT(ISODOW FROM "timestamp") NOT IN (6, 7)
        AND EXTRACT(HOUR FROM "timestamp") BETWEEN 9 AND 17
        AS within_business_hours,
    NOT (
        EXTRACT(ISODOW FROM "timestamp") NOT IN (6, 7)
        AND EXTRACT(HOUR FROM "timestamp") BETWEEN 9 AND 17
    ) AS review_flag_outside_business_hours
FROM feature_flag_audit_logs
ORDER BY "timestamp" DESC;

COMMENT ON VIEW feature_flag_audit_review IS
    'Governance review of feature flag changes: actor, before/after value, and a soft out-of-business-hours review signal. Retrospective only, not an approval gate.';
