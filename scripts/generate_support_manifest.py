#!/usr/bin/env python3
"""Classify every Phase 0 observed surface and attach target ownership/evidence."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from inventory_common import retained_revision


ROOT = Path(__file__).resolve().parents[1]
INPUT = ROOT / "tests/characterization/surface-golden.json"
OUTPUT = ROOT / "tests/characterization/support-manifest.json"
GENERATOR = "scripts/generate_support_manifest.py"


def shell_target(path: str) -> tuple[str, str]:
    if path.startswith("/auths/"):
        return "identity/security use cases", "rewrite"
    if path.startswith("/gateway/") or path.endswith("/signal"):
        return "realtime application port", "rewrite"
    if path.startswith("/machines/") or path.startswith("/programs/"):
        return "program/workload use cases and VMM port", "rewrite"
    if path.startswith("/stores/"):
        return "store/capsule use cases", "rewrite"
    if path == "/storage/upload":
        return "file/capsule use case", "rewrite"
    if "Finance" in path or any(
        word in path
        for word in ("Pool", "Hold", "Payout", "mint", "transfer", "payment")
    ):
        return "finance application use cases", "rewrite"
    if "/secret" in path:
        return "secret/capability use cases", "rewrite"
    if path.startswith("/api/"):
        return "HTTP health/diagnostic API", "deprecate"
    return "creature application use cases", "rewrite"


def http_target(surface: str) -> tuple[str, str]:
    if surface == "cluster-admin-and-raft":
        return "coordination/worker administration API", "delete-after-replacement"
    if surface == "vmm-http-ingress":
        return "gateway plus VMM HTTP forwarding", "rewrite"
    if surface == "public-storage-http":
        return "authenticated public file API", "rewrite"
    if "telemetry" in surface or "pprof" in surface:
        return "observability administration API", "move"
    return "canonical HTTP gateway", "rewrite"


def row(identifier: str, owner: str, target: str, disposition: str) -> dict[str, str]:
    return {
        "id": identifier,
        "support": "supported-compatibility",
        "current_owner": owner,
        "target_owner": target,
        "disposition": disposition,
        "characterization": "tests/characterization/surface-golden.json",
        "expiry": "ADR-0004 window after replacement acceptance",
    }


def build() -> dict[str, Any]:
    fixture = json.loads(INPUT.read_text(encoding="utf-8"))
    interfaces = fixture["interfaces"]

    shells = []
    for item in interfaces["signed_shell_actions"]:
        target, disposition = shell_target(item["path"])
        shells.append(row(item["path"], item["surface"], target, disposition))

    http = []
    for item in interfaces["http_routes"]:
        target, disposition = http_target(item["surface"])
        http.append(
            row(
                f"{item['method']} {item['path']}",
                item["surface"],
                target,
                disposition,
            )
        )

    guest = [
        row(
            item["operation"],
            item["gateway"],
            "authenticated guest gateway and typed application/VMM ports",
            "rewrite",
        )
        for item in interfaces["guest_operations"]
    ]

    runtimes = [
        row(
            item["key"],
            "compile-time caspar-vm-plugins aggregator",
            f"runtime/{item['key']} module behind VMM contract",
            "move-and-deprecate-aggregator",
        )
        for item in fixture["runtimes"]["providers"]
    ]

    cli: list[dict[str, str]] = []
    for group, commands in fixture["cli"]["casparctl"].items():
        for item in commands:
            target = "asemanctl"
            disposition = "deprecate-alias"
            if group == "cluster":
                target = "asemanctl node/worker/coordination commands"
                disposition = "rewrite-and-delete-openraft-command"
            cli.append(row(f"casparctl {group} {item['command']}", "casparctl", target, disposition))
    for item in fixture["cli"]["client_cli"]:
        cli.append(row(f"caspar-client {item['command']}", "apps/aseman-client", "generated Aseman client/asemanctl", "rewrite"))
    for item in fixture["cli"]["root_scripts"]:
        cli.append(row(item["script"], "root script", "xtask/bootstrap/deploy workflow", "rewrite"))

    return {
        "_meta": {
            "artifact": "A008",
            "status": "ACCEPTED",
            "last_verified_commit": retained_revision(ROOT, OUTPUT),
            "generator": GENERATOR,
            "policy": "Observed public behavior remains compatibility-supported unless an explicit intentional-removal row supersedes it.",
        },
        "signed_shell_actions": shells,
        "http_routes": http,
        "guest_operations": guest,
        "runtimes": runtimes,
        "cli_and_scripts": cli,
        "evidence": [
            "tests/characterization/test_surface_golden.py",
            "crates/aseman-contracts/src/legacy_gateway.rs::tests",
            "crates/aseman-contracts/src/legacy_storage_http.rs::tests",
            "apps/aseman-node/src/shell/storage_http.rs::characterization_tests",
            "cargo test -p aseman-node --lib (401 tests at Phase 0 baseline; migrated pure cases move to clean crates)",
            "cargo test -p asemanctl --all-targets (33 tests at baseline)",
        ],
        "limitations": [
            "The golden proves names, dispatch ownership, declared capabilities, and persistence shapes; phase contracts add deeper semantic and adversarial cases before each replacement.",
            "Supported-compatibility does not approve current security semantics; target authorization gates remain mandatory.",
        ],
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    value = json.dumps(build(), indent=2, sort_keys=True) + "\n"
    if args.check:
        if not OUTPUT.exists() or OUTPUT.read_text(encoding="utf-8") != value:
            print(f"stale: {OUTPUT.relative_to(ROOT)}", file=sys.stderr)
            return 1
        return 0
    OUTPUT.write_text(value, encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
