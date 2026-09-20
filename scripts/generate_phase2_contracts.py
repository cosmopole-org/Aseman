#!/usr/bin/env python3
"""Generate the Phase 2 module contract/capability catalog."""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import tomllib
from pathlib import Path

from inventory_common import retained_revision


ROOT = Path(__file__).resolve().parents[1]
MODULE = ROOT / "contracts/module"
JSON_OUT = ROOT / "docs/generated/module-contract-catalog.json"
MD_OUT = ROOT / "docs/generated/module-contract-catalog.md"
GENERATOR = "scripts/generate_phase2_contracts.py"


def relative(path: Path) -> str:
    return path.relative_to(ROOT).as_posix()


def digest(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def build() -> dict[str, object]:
    schema = json.loads((MODULE / "module.schema.json").read_text())
    compatibility = json.loads((MODULE / "protocol-compatibility.json").read_text())
    sample = tomllib.loads((MODULE / "fixtures/valid/sample-module.toml").read_text())
    contract_files = sorted(
        path
        for path in MODULE.rglob("*")
        if path.is_file() and path.suffix in {".json", ".proto", ".yaml"}
    )
    return {
        "_meta": {
            "artifact": "A201-A208",
            "status": "CURRENT",
            "generator": GENERATOR,
            "last_verified_commit": retained_revision(ROOT, JSON_OUT),
        },
        "protocol_major": compatibility["protocol_major"],
        "module_kinds": schema["properties"]["kind"]["enum"],
        "sample_capabilities": sample["capabilities"],
        "contracts": [
            {"path": relative(path), "digest": digest(path)} for path in contract_files
        ],
        "required_request_semantics": [
            "request_id", "trace_id", "deadline", "cancellation", "idempotency",
            "bounded_messages", "backpressure", "structured_errors"
        ],
    }


def markdown(data: dict[str, object]) -> str:
    lines = [
        "---",
        "status: GENERATED",
        "owner: module-platform",
        f"source_of_truth: contracts/module and {GENERATOR}",
        f"last_verified_commit: {str(data['_meta']['last_verified_commit'])[:12]}",
        f"verification: python3 {GENERATOR} --check",
        "---",
        "",
        "# Module contract and capability catalog",
        "",
        f"Protocol major: `{data['protocol_major']}`.",
        "",
        "## Module kinds",
        "",
    ]
    lines.extend(f"- `{kind}`" for kind in data["module_kinds"])
    lines += ["", "## Sample provider capabilities", ""]
    lines.extend(f"- `{capability}`" for capability in data["sample_capabilities"])
    lines += ["", "## Contract inputs", "", "| Path | SHA-256 |", "|---|---|"]
    lines.extend(f"| `{row['path']}` | `{row['digest']}` |" for row in data["contracts"])
    lines += ["", "All module RPCs use generated bindings and the required request semantics", "listed in the JSON catalog.", ""]
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
            print(f"wrote {relative(path)}")
    if stale:
        for path in stale:
            print(f"stale: {relative(path)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
