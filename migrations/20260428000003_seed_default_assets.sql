-- Seed default assets: USD, EUR, GBP with their canonical Stellar issuers
INSERT INTO assets (asset_code, asset_issuer, metadata, enabled)
VALUES
    ('USD', 'GDUKMGUGDZQK6YHYA5Z6AY2G4XDSZPSZ3SW9QRM65DKQE7GGKGL2JRWI',
     '{"description": "US Dollar (Circle/USDC anchor)"}', TRUE),
    ('EUR', 'GDHU6WRG4IEQXM5NZ4BMPKOXHW76MZM4Y2IEMFDVXBSDP6SJY4ITNPP',
     '{"description": "Euro (Tempo anchor)"}', TRUE),
    ('GBP', 'GCZJM35NKGVK47BB4SPBDV25477PZYIYPVVG453LPYFNXLS3FGHDXOCM',
     '{"description": "British Pound Sterling"}', TRUE)
ON CONFLICT (asset_code, asset_issuer) DO NOTHING;
