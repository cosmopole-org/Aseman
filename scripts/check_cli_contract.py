#!/usr/bin/env python3
"""Check the A903 CLI compatibility catalogue against generated dispatch inventory."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CONTRACT = ROOT / "contracts/cli/command-v1.json"
INVENTORY = ROOT / "docs/generated/current-cli-ops.json"


def check() -> list[str]:
    problems: list[str] = []
    contract = json.loads(CONTRACT.read_text(encoding="utf-8"))
    inventory = json.loads(INVENTORY.read_text(encoding="utf-8"))
    commands = contract.get("commands", [])
    names = [row.get("name") for row in commands]
    if len(names) != len(set(names)):
        problems.append("command names are not unique")
    actual = {row["command"] for row in inventory["casparctl"]["top_level"]}
    declared = set(names)
    if actual != declared:
        problems.append(
            f"dispatch/catalog mismatch: missing={sorted(actual-declared)}, stale={sorted(declared-actual)}"
        )
    modes = set(contract.get("output_modes", {}))
    for row in commands:
        if row.get("output") not in modes:
            problems.append(f"{row.get('name')} has unknown output mode {row.get('output')}")
        if row.get("class") == "alias" and row.get("alias_for") not in declared:
            problems.append(f"{row.get('name')} aliases an unknown command")
    if set(contract.get("exit_codes", {})) != {"0", "1", "2"}:
        problems.append("exit code catalogue must describe the implementation's exact 0/1/2 set")
    required = set(contract.get("json_error", {}).get("required", []))
    if required != {"schema", "ok", "command", "error"}:
        problems.append("the v1 JSON error envelope changed without a version bump")
    result_required = set(contract.get("json_result", {}).get("required", []))
    if result_required != {"schema", "ok", "command", "exit_code", "data", "warnings"}:
        problems.append("the v1 JSON result envelope changed without a version bump")
    if any(row.get("output") != "json_optional" for row in commands):
        problems.append("every top-level command must support the global --json envelope")
    source = (ROOT / "apps/asemanctl/src/cli/mod.rs").read_text()
    for marker in ["aseman.cli.result.v1", "aseman.cli.error.v1", "ASEMAN_CLI_STRUCTURED_CHILD"]:
        if marker not in source:
            problems.append(f"structured-output implementation lacks {marker}")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    parser.parse_args()
    problems = check()
    for problem in problems:
        print(f"CLI contract: {problem}", file=sys.stderr)
    if problems:
        return 1
    print("CLI compatibility catalogue holds")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
