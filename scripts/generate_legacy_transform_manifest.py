#!/usr/bin/env python3
"""Account for every A004 legacy access without inventing unreviewed transforms."""

from __future__ import annotations

import argparse
import json
from pathlib import Path

from inventory_common import retained_revision


ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "docs/generated/current-storage-access.json"
JSON_OUT = ROOT / "contracts/migration/legacy-transform-manifest.json"
MD_OUT = ROOT / "docs/generated/legacy-transform-manifest.md"
GENERATOR = "scripts/generate_legacy_transform_manifest.py"
OBJECT_TARGETS = {
    "Chain": "core.chain",
    "ChainShard": "core.chain_shard",
    "Creature": "core.creature",
    "Entity": "core.entity",
    "File": "core.file",
    "Program": "core.program",
    "Session": "core.session",
    "Store": "core.store",
}
DERIVED_METHODS = {
    "get_index", "put_index", "del_index", "has_index", "get_link", "put_link",
    "get_links_list", "search_link_vals_list", "search_link_keys_list_by_prefix",
}


def classify_access(row: dict[str, object]) -> dict[str, object]:
    template = row["logical_template"]
    method = row["method"]
    if method in DERIVED_METHODS or template.startswith(("link::", "index::")):
        status = "derived_index_or_relationship"
        target = None
        note = "rebuild from transformed capsules and verify against legacy value"
    elif template in OBJECT_TARGETS:
        status = "aggregate_with_object_family"
        target = OBJECT_TARGETS[template]
        note = "deduplicate with the object-column aggregate; never emit per access"
    elif template.startswith("obj::"):
        status = "aggregate_with_object_family"
        target = next(
            (kind for name, kind in OBJECT_TARGETS.items() if template.startswith(f"obj::{name}::")),
            None,
        )
        note = "object column must be assembled and transformed once"
    elif template.startswith("json::") or template.startswith("Json::") or "Meta::" in template:
        status = "blocked_payload_fixture"
        target = None
        note = "JSON shape and owner must be proven by fixtures before transformation"
    else:
        status = "blocked_semantic_review"
        target = None
        note = "caller ownership, retention, and target meaning are not yet proven"
    return {**row, "disposition": status, "target_kind": target, "note": note}


def build() -> dict[str, object]:
    source = json.loads(SOURCE.read_text())
    core_objects = []
    for row in source["core_objects"]:
        if row["object_type"] == "Program":
            disposition = "fixture_backed_transform"
            note = "strict columns; machineId resolves creature owner/relationship; unknown columns fail"
        elif row["object_type"] == "Creature":
            disposition = "fixture_backed_transform"
            note = "split into user/creature/wallet; RSA is tagged legacy multicodec; currency/scale required"
        elif row["object_type"] == "Entity":
            disposition = "fixture_backed_transform"
            note = "strict composite key and program relationship; program owner resolved server-side"
        elif row["object_type"] == "Store":
            disposition = "fixture_backed_transform"
            note = "strict binary fields; creator and optional parent links resolved from snapshot graph"
        elif row["object_type"] in {"Chain", "ChainShard"}:
            disposition = "fixture_backed_transform"
            note = "strict IDs; owner resolved through store creator; named shard identity preserved"
        elif row["object_type"] == "Session":
            disposition = "fixture_backed_transform"
            note = "legacy bearer ID becomes a domain-separated digest and immediately revoked session"
        elif row["object_type"] == "File":
            disposition = "fixture_backed_transform"
            note = "metadata emits only with bounded external-byte size and SHA-256 copy evidence"
        else:
            disposition = "requires_graph_transform"
            note = "assemble columns, resolve owner and relationships, validate required target fields"
        core_objects.append({
            **row,
            "target_kind": OBJECT_TARGETS[row["object_type"]],
            "disposition": disposition,
            "note": note,
        })
    questdb = []
    for row in source["questdb_tables"]:
        if row["name"] == "storage":
            disposition = "fixture_backed_transform"
            target_kind = "realtime.event"
            note = "rows sort into per-store sequences; scope/retention are server-resolved; tags are strict"
        elif row["name"] == "buildlogs":
            disposition = "fixture_backed_transform"
            target_kind = "telemetry.build_log"
            note = "VM owner is resolved server-side; milliseconds convert exactly to microseconds"
        else:
            disposition = "blocked_missing_target_kind"
            target_kind = None
            note = "no reviewed target kind exists"
        questdb.append({
            **row,
            "disposition": disposition,
            "target_kind": target_kind,
            "note": note,
        })
    hashgraph = [
        {
            **row,
            "disposition": "provider_checkpoint_export",
            "target_kind": "module.hashgraph.checkpoint",
            "note": "decode and verify through the future consensus-provider checkpoint contract",
        }
        for row in source["hashgraph_rocksdb"]
    ]
    cluster = {
        **source["cluster_rocksdb"],
        "disposition": "state_machine_export_then_retained_rollback_only",
        "target_kind": "core.control_plane_snapshot",
        "note": "OpenRaft logs/meta are not a second target authority; ADR 0012 governs removal",
    }
    accesses = [classify_access(row) for row in source["application_accesses"]]
    candidates = [
        {
            **row,
            "disposition": "blocked_candidate_review",
            "note": "heuristic source match is not sufficient evidence of a persisted key",
        }
        for row in source["candidate_key_templates"]
    ]
    blocked = sum(row["disposition"].startswith("blocked_") for row in accesses)
    blocked += len(candidates)
    blocked += sum(row["disposition"] != "fixture_backed_transform" for row in core_objects)
    blocked += sum(row["disposition"] != "fixture_backed_transform" for row in questdb)
    blocked += len(hashgraph)
    blocked += 1  # OpenRaft authority export remains governed by ADR 0012.
    return {
        "_meta": {
            "artifact": "A308",
            "status": "IN_PROGRESS" if blocked else "ACCEPTED",
            "generator": GENERATOR,
            "last_verified_commit": retained_revision(ROOT, JSON_OUT),
            "source_digest_artifact": "docs/generated/current-storage-access.json",
        },
        "schema_version": 1,
        "identity_strategy": "sha2-256-domain-separated-first-128-bits-with-legacy-id-map",
        "unknown_record_policy": "fail_closed",
        "physical_layouts": source["physical_layouts"],
        "core_objects": core_objects,
        "questdb_tables": questdb,
        "hashgraph_families": hashgraph,
        "cluster_rocksdb": cluster,
        "application_accesses": accesses,
        "candidate_key_templates": candidates,
        "summary": {
            "application_accesses": len(accesses),
            "candidate_key_templates": len(candidates),
            "blocked_rows": blocked,
        },
    }


def markdown(data: dict[str, object]) -> str:
    counts: dict[str, int] = {}
    for row in data["application_accesses"]:
        counts[row["disposition"]] = counts.get(row["disposition"], 0) + 1
    lines = [
        "---", "status: GENERATED", "owner: migration/storage",
        f"source_of_truth: docs/generated/current-storage-access.json and {GENERATOR}",
        f"last_verified_commit: {data['_meta']['last_verified_commit'][:12]}",
        f"verification: python3 {GENERATOR} --check", "---", "",
        "# Legacy-to-capsule transform manifest", "",
        f"A004 rows are exhaustively accounted for, but A308 remains `{data['_meta']['status']}`.",
        f"There are **{data['summary']['blocked_rows']}** blocked review rows. Unknown records fail",
        "closed; a blocked or heuristic row is never copied as an opaque authoritative capsule.", "",
        "## Direct access disposition", "", "| Disposition | Rows |", "|---|---:|",
    ]
    lines.extend(f"| `{name}` | {count} |" for name, count in sorted(counts.items()))
    lines += ["", "## Typed object families", "", "| Legacy family | Target | Status |", "|---|---|---|"]
    lines.extend(
        f"| `{row['object_type']}` | `{row['target_kind']}` | `{row['disposition']}` |"
        for row in data["core_objects"]
    )
    lines += [
        "", "The next review must add fixture-backed graph transforms for owners, relationships,",
        "JSON payloads, QuestDB signal history, Hashgraph checkpoints, and OpenRaft state-machine",
        "authority before A308 can be accepted or any backfill can start.", "",
    ]
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    data = build()
    outputs = {
        JSON_OUT: json.dumps(data, indent=2, sort_keys=True) + "\n",
        MD_OUT: markdown(data),
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
