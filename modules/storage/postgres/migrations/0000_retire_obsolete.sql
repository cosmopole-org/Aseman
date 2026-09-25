-- Runs before 0001 on every start. Drops core tables whose shape was retired while they
-- had no writer, so 0001 recreates them in the current shape. Idempotent: a table in
-- its current shape is never touched.

-- A401 (P4-01): identity keys replaced node keys, which never had a writer.
DO $$
BEGIN
  IF to_regclass('aseman_core.node_keys') IS NOT NULL THEN
    EXECUTE 'DROP TABLE aseman_core.node_keys';
  END IF;
END $$;

-- A403 (P4-03): capability grants gained subjects of every class, action sets,
-- selectors, delegation, and parents. The first shape (one `action` column, user-only
-- subjects) never had a writer.
DO $$
BEGIN
  IF EXISTS (
    SELECT 1 FROM information_schema.columns
    WHERE table_schema = 'aseman_core' AND table_name = 'capability_grants'
      AND column_name = 'action'
  ) THEN
    EXECUTE 'DROP TABLE aseman_core.capability_grants CASCADE';
  END IF;
END $$;
