-- Provisions the restricted role the application actually connects as.
--
-- Postgres docker images treat POSTGRES_USER as the initdb bootstrap
-- superuser, and superusers unconditionally bypass Row-Level Security
-- (rolbypassrls = true on that specific role — NOT something inherited by
-- roles it subsequently creates, which default to rolbypassrls = false
-- regardless of who creates them). Every environment this repo defines —
-- docker-compose.yml, docker-compose.dev.yml, and CI — connected the app
-- itself directly as that bootstrap `synapse` role, so every RLS policy in
-- this codebase (see migrations/20260501000000_tenant_rls.sql) was silently
-- ignored. This script runs once, on first container init (via
-- /docker-entrypoint-initdb.d), as that bootstrap superuser, to create a
-- separate, non-superuser, explicitly NOBYPASSRLS role and hand it
-- ownership of the schema so RLS policies are actually enforced against it
-- — including against rows it owns, since the RLS migration also sets
-- FORCE ROW LEVEL SECURITY.
--
-- docker-compose.yml / docker-compose.dev.yml then point the app's
-- DATABASE_URL at this role instead of the bootstrap superuser. The
-- equivalent for CI lives inline in .github/workflows/rust.yml (GitHub
-- Actions service containers don't support initdb.d volume mounts), and for
-- production it must be provisioned by whatever process manages the
-- production database — see docs/postmortem-cross-tenant-leak.md for the
-- rollout note on this.
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'synapse_app') THEN
        CREATE ROLE synapse_app WITH LOGIN PASSWORD 'synapse_app' NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS;
    END IF;
END
$$;

GRANT ALL PRIVILEGES ON DATABASE synapse TO synapse_app;
GRANT ALL ON SCHEMA public TO synapse_app;

-- Every new session for this role starts with app.is_admin = true by
-- default — the same default db::set_session_admin_context sets per
-- connection in application code (src/db/mod.rs), but set here at the role
-- level so it also covers any ad-hoc `psql`/`sqlx::PgPool::connect(...)`
-- session that isn't going through the app's own pool construction (e.g.
-- inline test suites), without needing every one of them to remember an
-- after_connect hook. The five customer-facing routes this fix scopes by
-- tenant still override this per-request via SET LOCAL
-- (queries::with_tenant), which is transaction-scoped and always wins over
-- this session-level default for the duration of that transaction.
ALTER ROLE synapse_app SET app.is_admin = 'true';
