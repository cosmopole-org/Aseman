-- ADR 0034: the capsule layout the database was last migrated to. Every connection
-- writes in this layout; the node's migration sets it from ASEMAN_STORAGE_CAPSULE_MODE.
CREATE TABLE IF NOT EXISTS aseman_core.storage_layout (
  singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
  layout TEXT NOT NULL CHECK (layout IN ('flattened', 'capsule')),
  changed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
REVOKE ALL ON aseman_core.storage_layout FROM PUBLIC;
