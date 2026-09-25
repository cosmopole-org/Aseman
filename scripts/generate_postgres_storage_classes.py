#!/usr/bin/env python3
"""Generate native PostgreSQL mappings for non-core capsule storage classes."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path

from inventory_common import retained_revision


ROOT = Path(__file__).resolve().parents[1]
KINDS = ROOT / "contracts/capsule/kinds"
POSTGRES = ROOT / "contracts/storage/postgres"
JSON_OUT = POSTGRES / "storage-class-mapping.json"
SQL_OUT = ROOT / "modules/storage/postgres/migrations/0002_storage_classes.sql"
MD_OUT = ROOT / "docs/generated/postgres-storage-class-mapping.md"
GENERATOR = "scripts/generate_postgres_storage_classes.py"
SCHEMAS = {
    "telemetry": "aseman_telemetry",
    "audit": "aseman_audit",
    "finance": "aseman_finance",
    "outbox": "aseman_outbox",
    "realtime": "aseman_realtime",
}
# ADR 0016: document fields travel in `capsule_cbor` and never receive a column.
DOCUMENT_TYPE = "document"
SQL_TYPES = {
    "bool": "BOOLEAN",
    "integer": "BIGINT",
    "float": "DOUBLE PRECISION",
    "bytes": "BYTEA",
    "text": "TEXT",
    "timestamp_micros": "BIGINT",
    "capsule_id": "UUID",
}
ENVELOPE_COLUMNS = {
    "id", "schema_version", "revision", "created_at_micros", "updated_at_micros",
    "previous_integrity", "integrity_hash", "owner_type", "owner_id", "owner_name",
    "tombstone", "capsule_cbor",
}
SAFE = re.compile(r"^[a-z][a-z0-9_]{0,62}$")


def quoted(identifier: str) -> str:
    if not SAFE.fullmatch(identifier):
        raise ValueError(f"unsafe PostgreSQL identifier: {identifier}")
    return f'"{identifier}"'


def bounded_name(prefix: str, table: str, fields: list[str]) -> str:
    source = "_".join([prefix, table, *fields])
    if len(source) <= 63:
        return source
    return source[:52] + "_" + hashlib.sha256(source.encode()).hexdigest()[:10]


def load() -> dict[str, object]:
    registry = json.loads((KINDS / "storage-class-registry.json").read_text())
    logical = json.loads((KINDS / "storage-class-logical-schemas.json").read_text())
    core = json.loads((KINDS / "core-registry.json").read_text())
    definitions = {row["kind"]: row for row in logical["definitions"]}
    targets = {
        row["kind"]: {"schema": "aseman_core", "table": row["table"]}
        for row in core["kinds"] if row["storage_class"] == "core"
    }
    targets.update({
        row["kind"]: {"schema": SCHEMAS[row["storage_class"]], "table": row["table"]}
        for row in registry["kinds"]
    })
    tables = []
    for registered in registry["kinds"]:
        kind = registered["kind"]
        definition = definitions[kind]
        relationships = {}
        for name, target_kind in definition["relationships"].items():
            if target_kind not in targets:
                raise ValueError(f"unknown relationship target {target_kind}")
            relationships[name] = {
                "target_kind": target_kind,
                "target_schema": targets[target_kind]["schema"],
                "target_table": targets[target_kind]["table"],
                "required": True,
                "on_delete": "restrict",
            }
        tables.append({
            "kind": kind,
            "schema": SCHEMAS[registered["storage_class"]],
            "table": registered["table"],
            "storage_class": registered["storage_class"],
            "consistency": registered["consistency"],
            "required_capabilities": registered["required_capabilities"],
            "fields": {
                name: field_type
                for name, field_type in definition["fields"].items()
                if field_type != DOCUMENT_TYPE
            },
            "field_columns": {
                name: (f"body_{name}" if name in ENVELOPE_COLUMNS else name)
                for name, field_type in definition["fields"].items()
                if field_type != DOCUMENT_TYPE
            },
            "document_fields": sorted(
                name
                for name, field_type in definition["fields"].items()
                if field_type == DOCUMENT_TYPE
            ),
            "required_fields": definition["required"],
            "relationships": relationships,
            "unique_indexes": definition["unique_indexes"],
            "range_indexes": definition["range_indexes"],
            "retention": definition["retention"],
            "mutation_policy": definition["mutation_policy"],
        })
    mapping = {"schema_version": 1, "schemas": list(SCHEMAS.values()), "tables": tables}
    validate(mapping)
    return mapping


def validate(mapping: dict[str, object]) -> None:
    seen = set()
    for row in mapping["tables"]:
        physical_table = (row["schema"], row["table"])
        if physical_table in seen:
            raise ValueError(f"duplicate native table: {physical_table}")
        seen.add(physical_table)
        names = [row["schema"], row["table"], *row["field_columns"].values(), *row["relationships"]]
        if any(not SAFE.fullmatch(name) for name in names):
            raise ValueError(f"unsafe identifier in {row['kind']}")
        if set(row["fields"]) != set(row["field_columns"]):
            raise ValueError(f"incomplete field mapping in {row['kind']}")
        physical = set(row["field_columns"].values())
        if len(physical) != len(row["fields"]) or physical & ENVELOPE_COLUMNS:
            raise ValueError(f"physical column collision in {row['kind']}")
        if not set(row["required_fields"]) <= set(row["fields"]) | set(row["document_fields"]):
            raise ValueError(f"undeclared required field in {row['kind']}")
        declared = set(row["fields"]) | set(row["relationships"])
        for index in [*row["unique_indexes"], *row["range_indexes"]]:
            if not index or not set(index) <= declared:
                raise ValueError(f"invalid index in {row['kind']}")
        if any(field_type not in SQL_TYPES for field_type in row["fields"].values()):
            raise ValueError(f"unsupported field type in {row['kind']}")


def physical_column(row: dict[str, object], logical: str) -> str:
    return row["field_columns"].get(logical, logical)


def ddl(mapping: dict[str, object]) -> str:
    lines = [
        f"-- Generated by {GENERATOR}; do not edit by hand.",
        "BEGIN;",
        "CREATE SCHEMA IF NOT EXISTS aseman_storage;",
        "REVOKE ALL ON SCHEMA aseman_storage FROM PUBLIC;",
    ]
    for schema in mapping["schemas"]:
        lines += [f"CREATE SCHEMA IF NOT EXISTS {schema};", f"REVOKE ALL ON SCHEMA {schema} FROM PUBLIC;"]
    lines += [
        "",
        "CREATE OR REPLACE FUNCTION aseman_storage.reject_capsule_mutation() RETURNS trigger",
        "LANGUAGE plpgsql AS $$ BEGIN",
        "  RAISE EXCEPTION 'append-only capsule rows cannot be updated or deleted' USING ERRCODE = '55000';",
        "END $$;",
        "",
    ]
    for row in mapping["tables"]:
        schema, table = row["schema"], row["table"]
        columns = [
            "  id UUID PRIMARY KEY",
            "  schema_version INTEGER NOT NULL CHECK (schema_version > 0)",
            "  revision BIGINT NOT NULL CHECK (revision > 0)",
            "  created_at_micros BIGINT NOT NULL",
            "  updated_at_micros BIGINT NOT NULL CHECK (updated_at_micros >= created_at_micros)",
            "  previous_integrity BYTEA",
            "  integrity_hash BYTEA NOT NULL CHECK (octet_length(integrity_hash) = 32)",
            "  owner_type TEXT NOT NULL",
            "  owner_id UUID",
            "  owner_name TEXT",
            "  tombstone BOOLEAN NOT NULL DEFAULT FALSE",
            "  capsule_cbor BYTEA NOT NULL",
        ]
        for name, field_type in row["fields"].items():
            columns.append(f"  {quoted(row['field_columns'][name])} {SQL_TYPES[field_type]}")
        for name, relationship in row["relationships"].items():
            columns.append(f"  {quoted(name)} UUID" + (" NOT NULL" if relationship["required"] else ""))
        checks = [
            "  CONSTRAINT ck_revision_chain CHECK ((revision = 1) = (previous_integrity IS NULL))",
            "  CONSTRAINT ck_previous_integrity CHECK (previous_integrity IS NULL OR octet_length(previous_integrity) = 32)",
            "  CONSTRAINT ck_owner_scope CHECK ((owner_type = 'global' AND owner_id IS NULL AND owner_name IS NULL) OR (owner_type IN ('node', 'creature') AND owner_id IS NOT NULL AND owner_name IS NULL) OR (owner_type = 'module' AND owner_id IS NULL AND owner_name IS NOT NULL))",
        ]
        checks.extend(
            f"  CONSTRAINT {bounded_name('ck_live', table, [name])} CHECK (tombstone OR {quoted(row['field_columns'][name])} IS NOT NULL)"
            for name in row["required_fields"]
            if name in row["field_columns"]
        )
        if row["mutation_policy"] == "append_only":
            checks.append("  CONSTRAINT ck_append_envelope CHECK (revision = 1 AND previous_integrity IS NULL AND NOT tombstone)")
        lines += [f"CREATE TABLE IF NOT EXISTS {schema}.{quoted(table)} (", ",\n".join([*columns, *checks]), ");", ""]

    for row in mapping["tables"]:
        schema, table = row["schema"], row["table"]
        for name, relationship in row["relationships"].items():
            constraint = bounded_name("fk", table, [name])
            lines += [
                f"ALTER TABLE {schema}.{quoted(table)} DROP CONSTRAINT IF EXISTS {constraint};",
                f"ALTER TABLE {schema}.{quoted(table)} ADD CONSTRAINT {constraint} FOREIGN KEY ({quoted(name)}) REFERENCES {relationship['target_schema']}.{quoted(relationship['target_table'])}(id) ON DELETE RESTRICT;",
            ]
        for fields in row["unique_indexes"]:
            index = bounded_name("uq", table, fields)
            columns = ", ".join(quoted(physical_column(row, field)) for field in fields)
            lines.append(f"CREATE UNIQUE INDEX IF NOT EXISTS {index} ON {schema}.{quoted(table)} ({columns}) WHERE NOT tombstone;")
        for fields in row["range_indexes"]:
            index = bounded_name("ix", table, fields)
            columns = ", ".join(quoted(physical_column(row, field)) for field in fields)
            lines.append(f"CREATE INDEX IF NOT EXISTS {index} ON {schema}.{quoted(table)} ({columns}, id) WHERE NOT tombstone;")
            if len(fields) == 1 and fields[0].endswith("_micros"):
                brin = bounded_name("brin", table, fields)
                lines.append(f"CREATE INDEX IF NOT EXISTS {brin} ON {schema}.{quoted(table)} USING BRIN ({columns});")
        if row["mutation_policy"] == "append_only":
            trigger = bounded_name("trg_immutable", table, [])
            lines += [
                f"DROP TRIGGER IF EXISTS {trigger} ON {schema}.{quoted(table)};",
                f"CREATE TRIGGER {trigger} BEFORE UPDATE OR DELETE ON {schema}.{quoted(table)} FOR EACH ROW EXECUTE FUNCTION aseman_storage.reject_capsule_mutation();",
            ]
        lines.append("")
    lines += ["COMMIT;", ""]
    return "\n".join(lines)


def markdown(mapping: dict[str, object]) -> str:
    lines = [
        "---", "status: GENERATED", "owner: storage/postgres",
        f"source_of_truth: contracts/capsule/kinds and {GENERATOR}",
        f"last_verified_commit: {retained_revision(ROOT, MD_OUT)[:12]}",
        f"verification: python3 {GENERATOR} --check", "---", "",
        "# PostgreSQL non-core storage-class mapping", "",
        "Each kind has a native typed table and preserves its canonical capsule envelope.",
        "Append-only, consistency, retention, and query-index policies are explicit; no",
        "universal payload table or JSONB entity bucket is used.", "",
        "| Kind | Native table | Consistency | Mutation | Retention |", "|---|---|---|---|---|",
    ]
    lines.extend(
        f"| `{row['kind']}` | `{row['schema']}.{row['table']}` | `{row['consistency']}` | `{row['mutation_policy']}` | `{row['retention']}` |"
        for row in mapping["tables"]
    )
    lines += ["", "Detailed finance, realtime, and retention behavior remains governed by their", "later application/provider contracts; this mapping cannot weaken A307 guarantees.", ""]
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    mapping = load()
    outputs = {
        JSON_OUT: json.dumps(mapping, indent=2, sort_keys=True) + "\n",
        SQL_OUT: ddl(mapping),
        MD_OUT: markdown(mapping),
    }
    stale = []
    for path, content in outputs.items():
        if args.check:
            if not path.exists() or path.read_text() != content:
                stale.append(path)
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
            print(f"wrote {path.relative_to(ROOT)}")
    if stale:
        for path in stale:
            print(f"stale: {path.relative_to(ROOT)}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
