-- The settlements.status column defaulted to 'pending', but
-- 20260502000000_settlement_dispute.sql later added a CHECK constraint whose
-- allowed set ('completed','pending_review','disputed','adjusted','voided')
-- does not include 'pending'. Any insert relying on the column default would
-- violate settlements_status_check. Align the default with the intended
-- initial state, 'pending_review'.
ALTER TABLE settlements ALTER COLUMN status SET DEFAULT 'pending_review';
