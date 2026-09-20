#!/usr/bin/env python3
"""Generate the first Phase 0 characterization fixture (A008).

The fixture freezes observable names, dispatch metadata, and persistence
shapes from the source-derived A002-A007 inventories.  It intentionally does
not decide which legacy behavior is supported; that acceptance remains a
separate review step documented beside the fixture.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from inventory_common import retained_revision


ROOT = Path(__file__).resolve().parents[1]
GENERATED = ROOT / "docs/generated"
FIXTURE_PATH = ROOT / "tests/characterization/surface-golden.json"
GENERATOR = "scripts/generate_characterization_fixtures.py"


def load(name: str) -> dict[str, Any]:
    return json.loads((GENERATED / name).read_text(encoding="utf-8"))


def without_source(value: Any) -> Any:
    """Remove diagnostic source locations from a behavioral golden value."""

    if isinstance(value, dict):
        return {
            key: without_source(item)
            for key, item in value.items()
            if key not in {"source", "config", "implementation"}
        }
    if isinstance(value, list):
        return [without_source(item) for item in value]
    return value


def build_fixture() -> dict[str, Any]:
    routes = load("current-routes.json")
    runtimes = load("current-runtime-matrix.json")
    cli = load("current-cli-ops.json")
    storage = load("current-storage-access.json")
    call_graph = load("current-call-graph.json")

    return {
        "_meta": {
            "artifact": "A008",
            "status": "ACCEPTED",
            "lifecycle_status": "VERIFIED",
            "scope": "observed interface, dispatch, runtime, CLI, and persistence shapes",
            "support_acceptance": "tests/characterization/support-manifest.json",
            "last_verified_commit": retained_revision(ROOT, FIXTURE_PATH),
            "generator": GENERATOR,
            "verification": (
                "python3 -m unittest discover -s tests/characterization "
                "-p 'test_*.py'"
            ),
            "inputs": [
                "docs/generated/current-routes.json",
                "docs/generated/current-runtime-matrix.json",
                "docs/generated/current-cli-ops.json",
                "docs/generated/current-storage-access.json",
                "docs/generated/current-call-graph.json",
            ],
        },
        "interfaces": {
            "signed_shell_actions": without_source(routes["signed_shell_actions"]),
            "http_routes": without_source(routes["http_routes"]),
            "guest_operations": without_source(routes["guest_operations"]),
        },
        "action_call_paths": without_source(call_graph["actions"]),
        "runtimes": {
            "trait_operations": runtimes["trait_operations"],
            "providers": without_source(runtimes["runtimes"]),
        },
        "cli": without_source(
            {
                "casparctl": cli["casparctl"],
                "client_cli": cli["client_cli"],
                "root_scripts": cli["root_scripts"],
            }
        ),
        "persistence": without_source(
            {
                "physical_layouts": storage["physical_layouts"],
                "core_objects": storage["core_objects"],
                "questdb_tables": storage["questdb_tables"],
                "hashgraph_rocksdb": storage["hashgraph_rocksdb"],
                "cluster_rocksdb": storage["cluster_rocksdb"],
            }
        ),
    }


def relative(path: Path) -> str:
    return path.relative_to(ROOT).as_posix()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    content = json.dumps(build_fixture(), indent=2, sort_keys=True) + "\n"

    if args.check:
        if not FIXTURE_PATH.exists() or FIXTURE_PATH.read_text(encoding="utf-8") != content:
            print(f"stale: {relative(FIXTURE_PATH)}", file=sys.stderr)
            return 1
        print("surface characterization fixture is up to date")
        return 0

    FIXTURE_PATH.parent.mkdir(parents=True, exist_ok=True)
    FIXTURE_PATH.write_text(content, encoding="utf-8")
    print(f"wrote {relative(FIXTURE_PATH)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
