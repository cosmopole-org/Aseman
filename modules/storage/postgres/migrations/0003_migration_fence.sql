-- A309 binding-generation fence (ADR 0006, plan 03 step 8). Hand-maintained: one row.
BEGIN;
CREATE TABLE IF NOT EXISTS aseman_core.migration_fence (
  singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
  min_generation BIGINT NOT NULL CHECK (min_generation >= 0)
);
INSERT INTO aseman_core.migration_fence (singleton, min_generation)
VALUES (TRUE, 0) ON CONFLICT (singleton) DO NOTHING;
COMMIT;
