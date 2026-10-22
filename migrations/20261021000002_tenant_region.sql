-- Per-tenant data residency region (issue #1516). NULL is rejected at ingress
-- (fail closed); it is never defaulted to a primary region.
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS region TEXT;
