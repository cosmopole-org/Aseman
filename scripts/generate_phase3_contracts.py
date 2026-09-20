#!/usr/bin/env python3
"""Generate the Phase 3 capsule contract and core-kind catalog."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

from inventory_common import retained_revision


ROOT = Path(__file__).resolve().parents[1]
CAPSULE = ROOT / "contracts/capsule"
JSON_OUT = ROOT / "docs/generated/capsule-contract-catalog.json"
MD_OUT = ROOT / "docs/generated/capsule-contract-catalog.md"
GENERATOR = "scripts/generate_phase3_contracts.py"


def digest(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def build() -> dict[str, object]:
    registry = json.loads((CAPSULE / "kinds/core-registry.json").read_text())
    files = sorted(path for path in CAPSULE.rglob("*") if path.is_file())
    return {
        "_meta": {
            "artifact": "A301-A304",
            "status": "ACCEPTED",
            "generator": GENERATOR,
            "last_verified_commit": retained_revision(ROOT, JSON_OUT),
        },
        "encoding": "deterministic-cbor-v1",
        "digest": "sha2-256",
        "core_kinds": registry["kinds"],
        "contracts": [
            {
                "path": path.relative_to(ROOT).as_posix(),
                "digest": digest(path),
            }
            for path in files
        ],
    }


def markdown(data: dict[str, object]) -> str:
    lines = [
        "---",
        "status: GENERATED",
        "owner: storage/application",
        f"source_of_truth: contracts/capsule and {GENERATOR}",
        f"last_verified_commit: {str(data['_meta']['last_verified_commit'])[:12]}",
        f"verification: python3 {GENERATOR} --check",
        "---",
        "",
        "# Capsule contract and core-kind catalog",
        "",
        f"Encoding: `{data['encoding']}`; integrity: `{data['digest']}`.",
        "",
        "| Kind | Native table | Class | Consistency | Owner |",
        "|---|---|---|---|---|",
    ]
    lines.extend(
        f"| `{row['kind']}` | `{row['table']}` | `{row['storage_class']}` | "
        f"`{row['consistency']}` | `{row['owner_scope']}` |"
        for row in data["core_kinds"]
    )
    lines += ["", "## Contract inputs", "", "| Path | SHA-256 |", "|---|---|"]
    lines.extend(f"| `{row['path']}` | `{row['digest']}` |" for row in data["contracts"])
    lines += [
        "",
        "P3-01 is accepted; providers must still pass the A310 behavioral conformance kit.",
        "",
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
    for path, value in outputs.items():
        if args.check:
            if not path.exists() or path.read_text() != value:
                stale.append(path)
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(value)
            print(f"wrote {path.relative_to(ROOT)}")
    if stale:
        for path in stale:
            print(f"stale: {path.relative_to(ROOT)}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
