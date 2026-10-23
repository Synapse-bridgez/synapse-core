-- Rollback: Tax lot tracking — Issue #1489
DROP TABLE IF EXISTS lot_consumption_pnl_map CASCADE;
DROP TABLE IF EXISTS realized_pnl CASCADE;
DROP TABLE IF EXISTS lot_consumptions CASCADE;
DROP TABLE IF EXISTS tax_lots CASCADE;
DROP FUNCTION IF EXISTS set_tax_lots_updated_at CASCADE;
