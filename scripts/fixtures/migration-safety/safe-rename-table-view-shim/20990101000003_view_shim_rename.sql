-- Fixture: create new table or view shim instead of renaming table under active writers.
CREATE TABLE IF NOT EXISTS settlements_v2 (
    id UUID PRIMARY KEY
);
