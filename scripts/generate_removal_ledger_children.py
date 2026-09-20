#!/usr/bin/env python3
"""Generate exhaustive Phase 0 child rows for removal-ledger artifact A011."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from inventory_common import retained_revision


ROOT = Path(__file__).resolve().parents[1]
GENERATED = ROOT / "docs/generated"
JSON_PATH = GENERATED / "removal-ledger-children.json"
MD_PATH = GENERATED / "removal-ledger-children.md"
GENERATOR = "scripts/generate_removal_ledger_children.py"


def load(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def storage_target(template: str) -> str:
    lower = template.lower()
    if any(word in lower for word in ("finance", "billing", "pool", "hold", "payout", "mint")):
        return "finance capsule/table mapping"
    if any(word in lower for word in ("vm", "machine", "program", "entity")):
        return "workload/program capsules and VMM observations"
    if any(word in lower for word in ("secret", "access", "grant", "login", "god")):
        return "identity/policy/secret capsule mapping"
    if any(word in lower for word in ("signal", "bridge", "callback")):
        return "realtime/outbox capsule mapping"
    if any(word in lower for word in ("creat", "user", "store")):
        return "typed core capsule/table mapping"
    return "reviewed A308 legacy-to-capsule transform"


def build() -> dict[str, Any]:
    config = load(GENERATED / "current-configuration.json")
    storage = load(GENERATED / "current-storage-access.json")
    support = load(ROOT / "tests/characterization/support-manifest.json")
    workspace = load(GENERATED / "current-workspace.json")

    config_rows = []
    for item in config["keys"]:
        legacy = item["key"].startswith("CASPAR_")
        config_rows.append(
            {
                "id": f"config:{item['key']}",
                "disposition": "deprecate-alias" if legacy else "move",
                "current_owner": item["category"],
                "callers": [entry["source"] for entry in item["occurrences"]],
                "target_owner": "typed AsemanConfig schema/composition root",
                "expiry": "ADR-0004 window" if legacy else "Phase 1 typed-config gate",
                "replacement_evidence": "A003/A103 config inventory, alias, and startup fixtures",
            }
        )

    unique_templates: dict[str, list[str]] = {}
    for item in storage["candidate_key_templates"]:
        unique_templates.setdefault(item["logical_template"], []).append(item["source"])
    storage_rows = []
    for template, callers in sorted(unique_templates.items()):
        storage_rows.append(
            {
                "id": f"storage-key:{template}",
                "disposition": "rewrite",
                "current_owner": "legacy RocksDB/transaction key space",
                "callers": sorted(callers),
                "target_owner": storage_target(template),
                "expiry": "Phase 3 cutover plus rollback window",
                "replacement_evidence": "A004/A301-A310 transform, semantic compare, rollback",
            }
        )
    for item in storage["physical_layouts"]:
        storage_rows.append(
            {
                "id": f"storage-layout:{item['family']}",
                "disposition": "rewrite",
                "current_owner": item["source"],
                "callers": [item["pattern"]],
                "target_owner": "capsule mapper and provider-native schema",
                "expiry": "Phase 3 deletion gate",
                "replacement_evidence": "A004/A308 transform and storage conformance",
            }
        )
    for item in storage["questdb_tables"]:
        storage_rows.append(
            {
                "id": f"questdb-table:{item['name']}",
                "disposition": "rewrite",
                "current_owner": item["source"],
                "callers": [op["source"] for op in item["operations"]],
                "target_owner": "telemetry/audit native PostgreSQL capsule table",
                "expiry": "Phase 3 deletion gate",
                "replacement_evidence": "A307-A310 migration/restart tests",
            }
        )
    for item in storage["hashgraph_rocksdb"]:
        storage_rows.append(
            {
                "id": f"hashgraph-key:{item['family']}",
                "disposition": "move",
                "current_owner": item["source"],
                "callers": [item["physical_pattern"]],
                "target_owner": "hashgraph consensus provider capsule mapping",
                "expiry": "Phase 8 provider gate",
                "replacement_evidence": "A804/A807 consensus and finance fixtures",
            }
        )
    for family in storage["cluster_rocksdb"]["column_families"]:
        storage_rows.append(
            {
                "id": f"openraft-column-family:{family}",
                "disposition": "delete-after-migration",
                "current_owner": storage["cluster_rocksdb"]["source"],
                "callers": [storage["cluster_rocksdb"]["path"]],
                "target_owner": "PostgreSQL capsule storage/CoordinationPort",
                "expiry": "Phase 6 HA gate plus ADR-0004 window",
                "replacement_evidence": "ADR 0012/0013 and A607 chaos fixtures",
            }
        )

    protocol_rows = []
    for group in ("signed_shell_actions", "http_routes", "guest_operations", "runtimes", "cli_and_scripts"):
        for item in support[group]:
            protocol_rows.append(
                {
                    "id": f"{group}:{item['id']}",
                    "disposition": item["disposition"],
                    "current_owner": item["current_owner"],
                    "callers": [item["id"]],
                    "target_owner": item["target_owner"],
                    "expiry": item["expiry"],
                    "replacement_evidence": item["characterization"],
                }
            )

    artifact_rows = []
    for package in workspace["rust"]["packages"]:
        artifact_rows.append(
            {
                "id": f"rust-package:{package['name']}",
                "disposition": "move-or-wrap-per-parent-ledger",
                "current_owner": package["manifest"],
                "callers": [dep["name"] for dep in package["dependencies"]],
                "target_owner": "root Aseman workspace or isolated legacy provider",
                "expiry": "owning phase replacement/deletion gate",
                "replacement_evidence": "A001 plus architecture dependency checks",
            }
        )
    for package in workspace["npm"]["packages"]:
        artifact_rows.append(
            {
                "id": f"npm-package:{package['name']}",
                "disposition": "rewrite/deprecate",
                "current_owner": package["manifest"],
                "callers": sorted(package["scripts"]),
                "target_owner": "generated Aseman SDK/client package",
                "expiry": "Phase 9/10 compatibility gate",
                "replacement_evidence": "A007/A701/A903 compatibility fixtures",
            }
        )

    return {
        "_meta": {
            "artifact": "A011",
            "status": "ACCEPTED",
            "last_verified_commit": retained_revision(ROOT, JSON_PATH),
            "generator": GENERATOR,
            "parent": "docs/migration/removal-ledger.md",
        },
        "configuration": config_rows,
        "storage": storage_rows,
        "protocol_and_compatibility": protocol_rows,
        "artifacts": artifact_rows,
        "summary": {
            "configuration": len(config_rows),
            "storage": len(storage_rows),
            "protocol_and_compatibility": len(protocol_rows),
            "artifacts": len(artifact_rows),
            "total": len(config_rows) + len(storage_rows) + len(protocol_rows) + len(artifact_rows),
        },
    }


def markdown(data: dict[str, Any]) -> str:
    summary = data["summary"]
    return "\n".join(
        [
            "---",
            "status: ACCEPTED",
            "owner: migration/P0-06",
            f"source_of_truth: {GENERATOR}",
            f"last_verified_commit: {data['_meta']['last_verified_commit'][:12]}",
            f"verification: python3 {GENERATOR} --check",
            "---",
            "",
            "# Generated removal-ledger child rows",
            "",
            "The machine-readable JSON contains owner, callers, target owner, disposition,",
            "expiry, and replacement evidence for every generated child row.",
            "",
            "| Class | Rows |",
            "|---|---:|",
            f"| Configuration keys | {summary['configuration']} |",
            f"| Storage layouts/key families | {summary['storage']} |",
            f"| Protocols, operations, runtimes, CLI, compatibility | {summary['protocol_and_compatibility']} |",
            f"| Package/artifact owners | {summary['artifacts']} |",
            f"| **Total** | **{summary['total']}** |",
            "",
            "No child row authorizes deletion. The parent ledger's replacement and deletion",
            "gates apply, and rows are retired only with phase-specific evidence.",
            "",
        ]
    )


def write_or_check(path: Path, value: str, check: bool) -> bool:
    if check:
        return path.exists() and path.read_text(encoding="utf-8") == value
    path.write_text(value, encoding="utf-8")
    return True


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    data = build()
    ok_json = write_or_check(JSON_PATH, json.dumps(data, indent=2, sort_keys=True) + "\n", args.check)
    ok_md = write_or_check(MD_PATH, markdown(data), args.check)
    if args.check and not (ok_json and ok_md):
        print("removal-ledger child rows are stale", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
