DROP POLICY IF EXISTS tenant_isolation ON settlements;

CREATE POLICY tenant_isolation ON settlements
    USING (
        current_setting('app.is_admin', true) = 'true'
        OR EXISTS (
            SELECT 1 FROM transactions t
            WHERE t.settlement_id = settlements.id
              AND (
                  t.tenant_id IS NULL
                  OR t.tenant_id::text = current_setting('app.tenant_id', true)
              )
        )
    );

DROP POLICY IF EXISTS tenant_isolation ON transactions;

CREATE POLICY tenant_isolation ON transactions
    USING (
        tenant_id IS NULL
        OR tenant_id::text = current_setting('app.tenant_id', true)
        OR current_setting('app.is_admin', true) = 'true'
    );
