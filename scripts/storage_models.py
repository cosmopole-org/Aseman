#!/usr/bin/env python3
"""Add or replace node models in the provider-neutral schemas (ADR 0036).

A model is written to `contracts/capsule/kinds/core-registry.json`,
`core-logical-schemas.json`, and (for its relations)
`contracts/storage/postgres/relationship-policies.json`, so every generator (the
PostgreSQL DDL, the storage client) picks it up. Used by the ADR 0036 model catalog
edits; idempotent.
"""

from __future__ import annotations

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REGISTRY = ROOT / "contracts/capsule/kinds/core-registry.json"
LOGICAL = ROOT / "contracts/capsule/kinds/core-logical-schemas.json"
POLICIES = ROOT / "contracts/storage/postgres/relationship-policies.json"


def _load(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def _save(path: Path, data: dict) -> None:
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")


def model(
    kind: str,
    table: str,
    fields: dict[str, str],
    *,
    required: list[str] | None = None,
    unique: list[list[str]] | None = None,
    relations: dict[str, tuple[str, bool, str]] | None = None,
    key_family: str | None = None,
    range_indexes: list[list[str]] | None = None,
    owner_scope: str = "global",
    owner: str | None = None,
    retention: str = "operational",
) -> None:
    relations = relations or {}
    unique = list(unique or [])
    fields = dict(fields)
    if key_family:
        fields = {"key": "text", **fields}
        if ["key"] not in unique:
            unique.insert(0, ["key"])
    required = list(dict.fromkeys((["key"] if key_family else []) + (required or [])))
    registry = _load(REGISTRY)
    registry["kinds"] = [row for row in registry["kinds"] if row["kind"] != kind]
    capabilities = ["indexes.unique"] if unique else []
    if relations:
        capabilities.insert(0, "relationships.foreign_keys")
    registry["kinds"].append(
        {
            "kind": kind,
            "table": table,
            "storage_class": "core",
            "consistency": "serializable",
            "owner_scope": owner_scope,
            "required_capabilities": capabilities,
        }
    )
    _save(REGISTRY, registry)
    logical = _load(LOGICAL)
    logical["definitions"] = [row for row in logical["definitions"] if row["kind"] != kind]
    definition = {
        "kind": kind,
        "fields": fields,
        "required": required,
        "unique_indexes": unique,
        "relationships": {name: target for name, (target, _, _) in relations.items()},
        "retention": retention,
    }
    if key_family:
        definition["key_family"] = key_family
    if range_indexes:
        definition["range_indexes"] = range_indexes
    if owner:
        definition["owner"] = owner
    logical["definitions"].append(definition)
    _save(LOGICAL, logical)
    policies = _load(POLICIES)
    policies["relationships"] = [
        row for row in policies["relationships"] if row["kind"] != kind
    ]
    for name, (_, needed, on_delete) in relations.items():
        policies["relationships"].append(
            {"kind": kind, "name": name, "required": needed, "on_delete": on_delete}
        )
    _save(POLICIES, policies)
