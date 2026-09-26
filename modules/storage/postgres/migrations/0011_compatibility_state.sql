-- Transitional storage for the node compatibility transaction surface.
--
-- This is deliberately not one key/value table.  Each legacy storage need has a
-- relational representation and an indexed compatibility key used only at the old
-- ITrx edge.  New application code must use typed ports and capsule mappings.
BEGIN;

CREATE SCHEMA IF NOT EXISTS aseman_compat;
REVOKE ALL ON SCHEMA aseman_compat FROM PUBLIC;

CREATE TABLE IF NOT EXISTS aseman_compat.object_columns (
    object_kind text NOT NULL CHECK (object_kind <> ''),
    object_id text NOT NULL CHECK (object_id <> ''),
    column_name text NOT NULL CHECK (column_name <> ''),
    value bytea NOT NULL,
    legacy_key text GENERATED ALWAYS AS (
        'obj::' || object_kind || '::' || object_id || '::' || column_name
    ) STORED,
    PRIMARY KEY (object_kind, object_id, column_name),
    UNIQUE (legacy_key)
);
CREATE INDEX IF NOT EXISTS compat_object_scan
    ON aseman_compat.object_columns (object_kind, object_id, column_name);
CREATE INDEX IF NOT EXISTS compat_object_key_prefix
    ON aseman_compat.object_columns (legacy_key text_pattern_ops);

CREATE TABLE IF NOT EXISTS aseman_compat.secondary_indexes (
    object_kind text NOT NULL CHECK (object_kind <> ''),
    from_column text NOT NULL CHECK (from_column <> ''),
    to_column text NOT NULL CHECK (to_column <> ''),
    from_value text NOT NULL,
    to_value bytea NOT NULL,
    legacy_key text GENERATED ALWAYS AS (
        'index::' || object_kind || '::' || from_column || '::' ||
        to_column || '::' || from_value
    ) STORED,
    PRIMARY KEY (object_kind, from_column, to_column, from_value),
    UNIQUE (legacy_key)
);
CREATE INDEX IF NOT EXISTS compat_secondary_lookup
    ON aseman_compat.secondary_indexes
       (object_kind, from_column, to_column, from_value text_pattern_ops);
CREATE INDEX IF NOT EXISTS compat_secondary_key_prefix
    ON aseman_compat.secondary_indexes (legacy_key text_pattern_ops);

-- A link group is a relation type plus its scope and member.  logical_key retains
-- exact legacy spelling only for compatibility reads; group scans use the structured
-- columns and their covering index.
CREATE TABLE IF NOT EXISTS aseman_compat.relations (
    relation_type text NOT NULL CHECK (relation_type <> ''),
    scope text NOT NULL,
    member text NOT NULL,
    logical_key text PRIMARY KEY,
    value text NOT NULL,
    legacy_key text GENERATED ALWAYS AS ('link::' || logical_key) STORED,
    UNIQUE (legacy_key),
    CHECK (
        logical_key = relation_type OR
        left(logical_key, length(relation_type) + 2) = relation_type || '::'
    )
);
CREATE INDEX IF NOT EXISTS compat_relation_group
    ON aseman_compat.relations (relation_type, scope, member) INCLUDE (value);
CREATE INDEX IF NOT EXISTS compat_relation_key_prefix
    ON aseman_compat.relations (legacy_key text_pattern_ops) INCLUDE (value);

-- JSONB is used only for genuinely document-shaped compatibility state.  The dotted
-- path remains a separate indexed dimension, so a subtree delete or lookup never scans
-- unrelated documents.
CREATE TABLE IF NOT EXISTS aseman_compat.documents (
    document_key text NOT NULL CHECK (document_key <> ''),
    path text NOT NULL CHECK (path <> ''),
    document jsonb NOT NULL,
    legacy_key text GENERATED ALWAYS AS (
        'json::' || document_key || '::' || path
    ) STORED,
    PRIMARY KEY (document_key, path),
    UNIQUE (legacy_key)
);
CREATE INDEX IF NOT EXISTS compat_document_tree
    ON aseman_compat.documents (document_key, path text_pattern_ops);
CREATE INDEX IF NOT EXISTS compat_document_key_prefix
    ON aseman_compat.documents (legacy_key text_pattern_ops);
CREATE INDEX IF NOT EXISTS compat_document_content
    ON aseman_compat.documents USING gin (document jsonb_path_ops);

-- Some compatibility callers still own unclassified operational bytes.  Keeping that
-- escape hatch isolated makes it measurable and deletable; structured objects,
-- indexes, relations, and documents are forbidden from falling into it.
CREATE TABLE IF NOT EXISTS aseman_compat.opaque_values (
    legacy_key text PRIMARY KEY CHECK (
        legacy_key NOT LIKE 'obj::%' AND
        legacy_key NOT LIKE 'index::%' AND
        legacy_key NOT LIKE 'link::%' AND
        legacy_key NOT LIKE 'json::%'
    ),
    value bytea NOT NULL
);
CREATE INDEX IF NOT EXISTS compat_opaque_key_prefix
    ON aseman_compat.opaque_values (legacy_key text_pattern_ops);

COMMIT;
