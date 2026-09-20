#!/usr/bin/env python3
"""Generate the Phase 0 legacy persistence inventory (A004).

The output separates proven physical layouts from heuristic key-template
candidates.  Candidate rows always retain a source location and never invent a
target capsule kind; semantic mapping is an explicit later acceptance step.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any

from inventory_common import retained_revision


ROOT = Path(__file__).resolve().parents[1]
JSON_PATH = ROOT / "docs/generated/current-storage-access.json"
MARKDOWN_PATH = ROOT / "docs/migration/legacy-data-map.md"
GENERATOR = "scripts/generate_legacy_data_inventory.py"
COMMIT = retained_revision(ROOT, JSON_PATH)

TRX_METHODS = (
    "del_key|get_by_prefix|has_obj|get_index|put_index|del_index|has_index|"
    "get_column|get_links_list|search_link_vals_list|search_link_keys_list_by_prefix|"
    "get_obj_list|get_link|put_link|put_bytes|get_bytes|put_string|get_string|"
    "get_obj|put_obj|put_json|del_json|get_json"
)


def rel(path: Path) -> str:
    return path.relative_to(ROOT).as_posix()


def line_number(value: str, offset: int) -> int:
    return value.count("\n", 0, offset) + 1


def loc(path: Path, value: str, offset: int) -> str:
    return f"{rel(path)}:{line_number(value, offset)}"


def production_source(path: Path) -> str:
    value = path.read_text(encoding="utf-8")
    test_at = value.find("#[cfg(test)]")
    return value if test_at < 0 else value[:test_at]


def rust_sources() -> list[Path]:
    paths = list((ROOT / "node/src").rglob("*.rs")) + list((ROOT / "vms").rglob("*.rs"))
    return sorted(
        path
        for path in paths
        if "tests.rs" not in path.name and "/target/" not in path.as_posix()
    )


def access_mode(method: str) -> str:
    if method.startswith(("put", "del")):
        return "write"
    return "read"


def logical_accesses() -> tuple[list[dict[str, str]], list[dict[str, str]]]:
    direct: list[dict[str, str]] = []
    candidates: list[dict[str, str]] = []
    direct_pattern = re.compile(
        rf"\.(?P<method>{TRX_METHODS})\(\s*&?\s*(?:format!\(\s*)?\"(?P<key>[^\"]+)\"",
        re.S,
    )
    format_pattern = re.compile(r'format!\(\s*"([^\"]*::[^\"]*)"')
    for path in rust_sources():
        value = production_source(path)
        for found in direct_pattern.finditer(value):
            key = found.group("key")
            method = found.group("method")
            direct.append(
                {
                    "method": method,
                    "mode": access_mode(method),
                    "logical_template": key,
                    "source": loc(path, value, found.start()),
                }
            )
        for found in format_pattern.finditer(value):
            candidates.append(
                {
                    "logical_template": found.group(1),
                    "source": loc(path, value, found.start()),
                    "review_status": "candidate; caller association may be indirect",
                }
            )

    direct_unique = {
        (row["method"], row["logical_template"], row["source"]): row for row in direct
    }
    candidate_unique = {
        (row["logical_template"], row["source"]): row for row in candidates
    }
    return (
        sorted(direct_unique.values(), key=lambda row: (row["logical_template"], row["source"])),
        sorted(candidate_unique.values(), key=lambda row: (row["logical_template"], row["source"])),
    )


def core_objects() -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    model_dir = ROOT / "node/src/shell/api/model"
    for path in sorted(model_dir.glob("*.rs")):
        value = production_source(path)
        for found in re.finditer(
            r"impl\s+([A-Za-z0-9_]+)\s*\{(?P<body>.*?)(?=\n}\n)", value, re.S
        ):
            rust_type = found.group(1)
            body = found.group("body")
            type_found = re.search(r'pub fn type_\(\).*?\{\s*"([^\"]+)"', body, re.S)
            if not type_found or "put_obj" not in body:
                continue
            columns = sorted(set(re.findall(r'cols\.insert\(\s*"([^\"]+)"', body)))
            rows.append(
                {
                    "object_type": type_found.group(1),
                    "rust_type": rust_type,
                    "physical_pattern": f"obj::{type_found.group(1)}::{{id}}::{{column}}",
                    "columns": columns,
                    "source": loc(path, value, found.start()),
                }
            )
    return sorted(rows, key=lambda row: row["object_type"])


def questdb_tables() -> list[dict[str, Any]]:
    path = ROOT / "node/src/drivers/storage.rs"
    value = production_source(path)
    creates: dict[str, dict[str, Any]] = {}
    for found in re.finditer(
        r"create table(?: if not exists)?\s+([a-zA-Z0-9_]+)\s*\(([^;]+)\)", value, re.I
    ):
        name = found.group(1).lower()
        columns = []
        for definition in found.group(2).split(","):
            parts = definition.strip().split()
            if len(parts) >= 2:
                columns.append({"name": parts[0], "type": parts[1].lower()})
        creates[name] = {
            "name": name,
            "columns": columns,
            "source": loc(path, value, found.start()),
            "operations": [],
        }
    sql_pattern = re.compile(r'"((?:INSERT INTO|update|SELECT .*? FROM)\s+[^\"]+)"', re.I)
    for found in sql_pattern.finditer(value):
        sql = found.group(1)
        table_match = re.search(r"(?:INTO|update|FROM)\s+([a-zA-Z0-9_]+)", sql, re.I)
        if not table_match:
            continue
        name = table_match.group(1).lower()
        if name in creates:
            creates[name]["operations"].append(
                {
                    "kind": sql.split(None, 1)[0].upper(),
                    "source": loc(path, value, found.start()),
                }
            )
    return [creates[name] for name in sorted(creates)]


def hashgraph_families() -> list[dict[str, str]]:
    path = ROOT / "node/src/drivers/network/chain/hashgraph/rocks_store.rs"
    value = production_source(path)
    rows = [
        ("repertoire", "rep_{public_key}", "Peer marshal bytes"),
        ("peer-set", "peerset_{round:09}", "PeerSet marshal bytes"),
        ("topological-event", "topo_{index:09}", "Event marshal bytes"),
        ("participant-event", "{participant}__event_{index:09}", "Event marshal bytes"),
        ("participant-root", "{participant}_root", "Root marshal bytes"),
        ("round", "round_{index:09}", "RoundInfo marshal bytes"),
        ("block", "block_{index:09}", "Block marshal bytes"),
        ("frame", "frame_{index:09}", "Frame marshal bytes; retention applies"),
    ]
    return [
        {
            "family": family,
            "physical_pattern": pattern,
            "value_shape": shape,
            "database": "separate Hashgraph RocksDB",
            "source": f"{rel(path)}:35",
        }
        for family, pattern, shape in rows
    ]


def cluster_store() -> dict[str, Any]:
    path = ROOT / "node/src/drivers/cluster/store.rs"
    value = production_source(path)
    cfs = re.findall(r'ColumnFamilyDescriptor::new\("([^\"]+)"', value)
    metadata = sorted(set(re.findall(r'(?:get_meta|put_meta)\("([^\"]+)"', value)))
    return {
        "database": "OpenRaft RocksDB",
        "path": "<storage_root>/cluster/raft-db",
        "column_families": cfs,
        "metadata_keys": metadata,
        "log_key": "big-endian u64 log index in logs column family",
        "serialization": "JSON values; binary log keys",
        "source": f"{rel(path)}:1",
    }


def inventory() -> dict[str, Any]:
    direct, candidates = logical_accesses()
    objects = core_objects()
    tables = questdb_tables()
    return {
        "_meta": {
            "artifact": "A004",
            "status": "CURRENT",
            "lifecycle_status": "GENERATED",
            "source_of_truth": "legacy transaction/storage/model/consensus source",
            "last_verified_commit": COMMIT,
            "verification": f"python3 {GENERATOR} --check",
            "generator": GENERATOR,
        },
        "physical_layouts": [
            {
                "family": "object-column",
                "pattern": "obj::{type}::{object_id}::{column}",
                "source": "node/src/core/actor/model/trx.rs:247",
            },
            {
                "family": "secondary-index",
                "pattern": "index::{type}::{from_column}::{to_column}::{from_value}",
                "source": "node/src/core/actor/model/trx.rs:264",
            },
            {
                "family": "link",
                "pattern": "link::{logical_key}",
                "source": "node/src/core/actor/model/trx.rs:325",
            },
            {
                "family": "json-document-and-leaves",
                "pattern": "json::{logical_key}::{path}[.{descendant_path}]",
                "source": "node/src/core/actor/model/trx.rs:430",
            },
            {
                "family": "raw",
                "pattern": "caller-defined byte/string key",
                "source": "node/src/core/actor/model/trx.rs:339",
            },
        ],
        "core_objects": objects,
        "application_accesses": direct,
        "candidate_key_templates": candidates,
        "questdb_tables": tables,
        "hashgraph_rocksdb": hashgraph_families(),
        "cluster_rocksdb": cluster_store(),
        "summary": {
            "core_object_types": len(objects),
            "direct_application_accesses": len(direct),
            "candidate_key_templates": len(candidates),
            "questdb_tables": len(tables),
            "hashgraph_key_families": 8,
        },
        "unresolved": [
            "Dynamic keys passed through variables need caller-by-caller semantic ownership review.",
            "JSON payload schemas are not uniformly typed and require fixtures before capsule transforms.",
            "Guest-visible dbOp calls require ownership analysis before migration to ADR 0001's signed proxy and trusted creature database/role binding.",
            "Filesystem artifacts and deployed entity blobs require a separate ownership/retention pass.",
            "No target capsule kind is assigned until canonical capsule/consistency ADRs are accepted.",
        ],
    }


def markdown(data: dict[str, Any]) -> str:
    summary = data["summary"]
    lines = [
        "---",
        "status: CURRENT",
        "owner: migration/P0-01",
        "source_of_truth: legacy transaction/storage/model/consensus source",
        f"last_verified_commit: {COMMIT}",
        f"verification: python3 {GENERATOR} --check",
        "---",
        "",
        "# Legacy data map",
        "",
        f"> Generated by `{GENERATOR}`. Do not edit by hand.",
        "",
        "This is the A004 current-state map. It records physical layouts and access",
        "sites without assigning speculative capsule kinds. The full source-location",
        "inventory is in `docs/generated/current-storage-access.json`.",
        "",
        "## Physical application RocksDB layouts",
        "",
        "| Family | Physical pattern | Source |",
        "|---|---|---|",
    ]
    for row in data["physical_layouts"]:
        lines.append(f"| {row['family']} | `{row['pattern']}` | `{row['source']}` |")

    lines += [
        "",
        "The transaction wrapper is a write-back overlay committed as one RocksDB batch.",
        "Application commits may also be proposed to the embedded OpenRaft cluster.",
        "",
        "## Typed object families",
        "",
        "| Current object type | Columns observed in model writer | Physical pattern |",
        "|---|---|---|",
    ]
    for row in data["core_objects"]:
        columns = ", ".join(f"`{column}`" for column in row["columns"]) or "—"
        lines.append(f"| `{row['object_type']}` | {columns} | `{row['physical_pattern']}` |")

    lines += [
        "",
        "## QuestDB tables",
        "",
        "| Table | Columns | Current role |",
        "|---|---|---|",
    ]
    roles = {
        "storage": "store signal history",
        "buildlogs": "VM/build/runtime logs",
    }
    for row in data["questdb_tables"]:
        columns = ", ".join(f"`{column['name']} {column['type']}`" for column in row["columns"])
        lines.append(f"| `{row['name']}` | {columns} | {roles.get(row['name'], 'unclassified')} |")

    lines += [
        "",
        "## Other RocksDB authorities",
        "",
        "- Hashgraph uses a separate database with eight key families: repertoire, peer sets, topological and participant events, roots, rounds, blocks, and frames.",
        "- OpenRaft uses `<storage_root>/cluster/raft-db` with `meta`, `logs`, and `sm` column families.",
        "- These stores have distinct current responsibilities and must not be treated as one interchangeable consensus system.",
        "",
        "## Access evidence",
        "",
        f"- Direct transaction accesses with a recoverable literal/template: {summary['direct_application_accesses']}",
        f"- Additional formatted key-template candidates requiring semantic review: {summary['candidate_key_templates']}",
        "- Every row includes its reader/writer method and source location in the JSON artifact.",
        "",
        "## Unresolved before capsule mapping",
        "",
    ]
    lines.extend(f"- {item}" for item in data["unresolved"])
    lines.append("")
    return "\n".join(lines)


def outputs() -> dict[Path, str]:
    data = inventory()
    return {
        JSON_PATH: json.dumps(data, indent=2, sort_keys=True) + "\n",
        MARKDOWN_PATH: markdown(data),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    records = outputs()
    if args.check:
        stale = [path for path, value in records.items() if not path.exists() or path.read_text() != value]
        if stale:
            for path in stale:
                print(f"stale: {rel(path)}", file=sys.stderr)
            return 1
        print("legacy data inventory is up to date")
        return 0
    for path, value in records.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(value, encoding="utf-8")
        print(f"wrote {rel(path)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
