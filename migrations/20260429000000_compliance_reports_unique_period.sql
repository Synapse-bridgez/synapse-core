ALTER TABLE compliance_reports
    ADD CONSTRAINT uq_compliance_reports_period_start UNIQUE (period, period_start);
