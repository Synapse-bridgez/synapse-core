-- Insert sample tenants for testing
INSERT INTO tenants (
    tenant_id,
    name,
    api_key,
    webhook_secret,
    stellar_account,
    rate_limit_per_minute,
    is_active
) VALUES 
(
    '11111111-1111-1111-1111-111111111111',
    'Anchor Platform Demo',
    'demo_api_key_anchor_platform_001',
    'webhook_secret_demo_001',
    'GDEMOACCOUNT1111111111111111111111111111111111111',
    100,
    true
),
(
    '22222222-2222-2222-2222-222222222222',
    'Partner Integration Test',
    'test_api_key_partner_002',
    'webhook_secret_test_002',
    'GTESTACCOUNT2222222222222222222222222222222222222',
    60,
    true
),
(
    '33333333-3333-3333-3333-333333333333',
    'Inactive Tenant',
    'inactive_api_key_003',
    'webhook_secret_inactive_003',
    'GINACTIVEACCT333333333333333333333333333333333333',
    30,
    false
)
ON CONFLICT (api_key) DO NOTHING;

-- Add comment
COMMENT ON TABLE tenants IS 'Sample tenants added for development and testing';
