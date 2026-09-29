-- Drop webhook redirect tables and their RLS policies
DROP POLICY IF EXISTS webhook_redirects_tenant_isolation ON webhook_endpoint_redirects;
DROP TABLE IF EXISTS webhook_redirect_deliveries;
DROP TABLE IF EXISTS webhook_endpoint_redirects;
