#!/usr/bin/env python3
"""Generate the native PostgreSQL core mapping, migration, and documentation."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path

from inventory_common import retained_revision


ROOT = Path(__file__).resolve().parents[1]
CAPSULE = ROOT / "contracts/capsule/kinds"
POSTGRES = ROOT / "contracts/storage/postgres"
JSON_OUT = POSTGRES / "core-mapping.json"
SQL_OUT = ROOT / "modules/storage/postgres/migrations/0001_core.sql"
MD_OUT = ROOT / "docs/generated/postgres-core-mapping.md"
GENERATOR = "scripts/generate_postgres_core.py"
SCHEMA = "aseman_core"
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
    "id",
    "schema_version",
    "revision",
    "created_at_micros",
    "updated_at_micros",
    "previous_integrity",
    "integrity_hash",
    "owner_type",
    "owner_id",
    "owner_name",
    "tombstone",
    "capsule_cbor",
}
SAFE = re.compile(r"^[a-z][a-z0-9_]{0,62}$")


def quoted(identifier: str) -> str:
    if not SAFE.fullmatch(identifier):
        raise ValueError(f"unsafe PostgreSQL identifier: {identifier}")
    return f'"{identifier}"'


def load() -> tuple[dict[str, object], list[dict[str, object]]]:
    registry = json.loads((CAPSULE / "core-registry.json").read_text())
    logical = json.loads((CAPSULE / "core-logical-schemas.json").read_text())
    policies = json.loads((POSTGRES / "relationship-policies.json").read_text())
    kinds = {row["kind"]: row for row in registry["kinds"]}
    definitions = {row["kind"]: row for row in logical["definitions"]}
    policy = {(row["kind"], row["name"]): row for row in policies["relationships"]}
    core_kinds = [row for row in registry["kinds"] if row["storage_class"] == "core"]
    tables = []
    observed_relationships: set[tuple[str, str]] = set()
    for registered in core_kinds:
        kind = registered["kind"]
        definition = definitions[kind]
        relationships = {}
        for name, target_kind in definition["relationships"].items():
            key = (kind, name)
            if key not in policy:
                raise ValueError(f"missing relationship policy for {kind}.{name}")
            observed_relationships.add(key)
            row = policy[key]
            if target_kind not in kinds:
                raise ValueError(f"unknown relationship target {target_kind}")
            relationships[name] = {
                "target_kind": target_kind,
                "target_table": kinds[target_kind]["table"],
                "required": row["required"],
                "on_delete": row["on_delete"],
            }
        document_fields = sorted(
            name
            for name, field_type in definition["fields"].items()
            if field_type == DOCUMENT_TYPE
        )
        columnar = {
            name: field_type
            for name, field_type in definition["fields"].items()
            if field_type != DOCUMENT_TYPE
        }
        tables.append(
            {
                "kind": kind,
                "table": registered["table"],
                "fields": columnar,
                "field_columns": {
                    name: (f"body_{name}" if name in ENVELOPE_COLUMNS else name)
                    for name in columnar
                },
                "document_fields": document_fields,
                "required_fields": definition["required"],
                "relationships": relationships,
                "unique_indexes": definition["unique_indexes"],
            }
        )
    if observed_relationships != set(policy):
        extra = sorted(set(policy) - observed_relationships)
        raise ValueError(f"relationship policies not used by core mapping: {extra}")
    mapping = {"schema_version": 1, "schema": SCHEMA, "tables": tables}
    return mapping, core_kinds


def validate(mapping: dict[str, object]) -> None:
    table_names = {row["table"] for row in mapping["tables"]}
    if len(table_names) != len(mapping["tables"]):
        raise ValueError("native table names must be unique")
    for row in mapping["tables"]:
        names = [row["table"], *row["field_columns"].values(), *row["relationships"]]
        for name in names:
            if not SAFE.fullmatch(name):
                raise ValueError(f"unsafe PostgreSQL identifier: {name}")
        if set(row["fields"]) & set(row["relationships"]):
            raise ValueError(f"field/relationship collision in {row['kind']}")
        if set(row["field_columns"]) != set(row["fields"]):
            raise ValueError(f"field column mapping is incomplete in {row['kind']}")
        physical = set(row["field_columns"].values())
        if (
            len(physical) != len(row["fields"])
            or physical & ENVELOPE_COLUMNS
            or physical & set(row["relationships"])
        ):
            raise ValueError(f"physical column collision in {row['kind']}")
        for field_type in row["fields"].values():
            if field_type not in SQL_TYPES:
                raise ValueError(f"unsupported field type: {field_type}")
        if set(row["document_fields"]) & set(row["fields"]):
            raise ValueError(f"document field also has a column in {row['kind']}")
        if not set(row["required_fields"]) <= set(row["fields"]) | set(row["document_fields"]):
            raise ValueError(f"required field is undeclared in {row['kind']}")
        declared = set(row["fields"]) | set(row["relationships"])
        for index in row["unique_indexes"]:
            if not set(index) <= declared:
                raise ValueError(f"index references unknown column in {row['kind']}")
        for relationship in row["relationships"].values():
            if relationship["target_table"] not in table_names:
                raise ValueError(
                    f"core table {row['table']} targets a table outside the core mapping"
                )


def constraint_name(prefix: str, table: str, fields: list[str]) -> str:
    source = "_".join([prefix, table, *fields])
    if len(source) <= 63:
        return source
    digest = hashlib.sha256(source.encode()).hexdigest()[:10]
    return source[:52] + "_" + digest


def ddl(mapping: dict[str, object]) -> str:
    lines = [
        "-- Generated by scripts/generate_postgres_core.py; do not edit by hand.",
        "BEGIN;",
        f"CREATE SCHEMA IF NOT EXISTS {SCHEMA};",
        f"REVOKE ALL ON SCHEMA {SCHEMA} FROM PUBLIC;",
        "",
    ]
    for row in mapping["tables"]:
        table = row["table"]
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
        body_checks = []
        for name, field_type in row["fields"].items():
            columns.append(
                f"  {quoted(row['field_columns'][name])} {SQL_TYPES[field_type]}"
            )
            body_checks.append(name)
        for name, relationship in row["relationships"].items():
            nullable = " NOT NULL" if relationship["required"] else ""
            columns.append(f"  {quoted(name)} UUID{nullable}")
        checks = [
            "  CONSTRAINT ck_revision_chain CHECK ((revision = 1) = (previous_integrity IS NULL))",
            "  CONSTRAINT ck_previous_integrity CHECK (previous_integrity IS NULL OR octet_length(previous_integrity) = 32)",
            "  CONSTRAINT ck_owner_scope CHECK ((owner_type = 'global' AND owner_id IS NULL AND owner_name IS NULL) OR (owner_type IN ('node', 'creature') AND owner_id IS NOT NULL AND owner_name IS NULL) OR (owner_type = 'module' AND owner_id IS NULL AND owner_name IS NOT NULL))",
        ]
        checks.extend(
            f"  CONSTRAINT {constraint_name('ck_live', table, [name])} CHECK (tombstone OR {quoted(row['field_columns'][name])} IS NOT NULL)"
            for name in body_checks
            if name in row["required_fields"]
        )
        lines.append(f"CREATE TABLE IF NOT EXISTS {SCHEMA}.{quoted(table)} (")
        lines.append(",\n".join([*columns, *checks]))
        lines.append(");")
        lines.append("")

    for row in mapping["tables"]:
        table = row["table"]
        for name, relationship in row["relationships"].items():
            fk = constraint_name("fk", table, [name])
            action = relationship["on_delete"].upper()
            lines.append(
                f"ALTER TABLE {SCHEMA}.{quoted(table)} DROP CONSTRAINT IF EXISTS {fk};"
            )
            lines.append(
                f"ALTER TABLE {SCHEMA}.{quoted(table)} ADD CONSTRAINT {fk} FOREIGN KEY ({quoted(name)}) "
                f"REFERENCES {SCHEMA}.{quoted(relationship['target_table'])}(id) ON DELETE {action};"
            )
        for fields in row["unique_indexes"]:
            name = constraint_name("uq", table, fields)
            columns = ", ".join(
                quoted(row["field_columns"].get(field, field)) for field in fields
            )
            lines.append(
                f"CREATE UNIQUE INDEX IF NOT EXISTS {name} ON {SCHEMA}.{quoted(table)} ({columns}) WHERE NOT tombstone;"
            )
        lines.append(
            f"CREATE INDEX IF NOT EXISTS {constraint_name('ix', table, ['updated_at'])} "
            f"ON {SCHEMA}.{quoted(table)} (updated_at_micros, id);"
        )
        lines.append("")
    lines += ["COMMIT;", ""]
    return "\n".join(lines)


def markdown(mapping: dict[str, object]) -> str:
    lines = [
        "---",
        "status: GENERATED",
        "owner: storage/postgres",
        f"source_of_truth: contracts/capsule/kinds, contracts/storage/postgres, and {GENERATOR}",
        f"last_verified_commit: {retained_revision(ROOT, MD_OUT)[:12]}",
        f"verification: python3 {GENERATOR} --check",
        "---",
        "",
        "# PostgreSQL core mapping",
        "",
        "Every core kind has its own native table in `aseman_core`. `capsule_cbor` preserves",
        "the signed canonical envelope while typed columns, foreign keys, partial unique",
        "indexes, and checks enforce the accepted logical schema. No guest payload table exists.",
        "",
        "| Kind | Table | Typed fields | Relationships | Unique indexes |",
        "|---|---|---:|---:|---:|",
    ]
    for row in mapping["tables"]:
        lines.append(
            f"| `{row['kind']}` | `{SCHEMA}.{row['table']}` | {len(row['fields'])} | "
            f"{len(row['relationships'])} | {len(row['unique_indexes'])} |"
        )
    lines += [
        "",
        "The guest catalog tables contain only trusted bindings and schema definitions.",
        "Creature-owned rows are stored later in separate provider-native databases/namespaces",
        "under dedicated roles; they are never placed in a shared `guest_capsules` table.",
        "",
    ]
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    mapping, _ = load()
    validate(mapping)
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
