#!/usr/bin/env python3
"""Validate that A1002 scenarios have runnable owners and numeric release thresholds."""

from __future__ import annotations

import json
import shlex
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "contracts/testing/operational-scenarios-v1.json"


def fail(message: str) -> None:
    raise SystemExit(f"operational scenarios: {message}")


def main() -> None:
    data = json.loads(MANIFEST.read_text())
    if data.get("artifact") != "A1002" or data.get("version") != 1:
        fail("unsupported manifest identity/version")
    scenarios = data.get("scenarios", [])
    ids = [scenario.get("id") for scenario in scenarios]
    if not scenarios or len(ids) != len(set(ids)):
        fail("scenario IDs must be present and unique")
    kinds = {scenario.get("kind") for scenario in scenarios}
    required_kinds = {"fuzz", "load", "soak", "chaos", "failover", "rollback"}
    if not required_kinds <= kinds:
        fail(f"missing scenario kinds: {sorted(required_kinds - kinds)}")
    load_targets = {
        target
        for scenario in scenarios
        if scenario.get("kind") in {"load", "soak"}
        for target in scenario.get("targets", [])
    }
    required_targets = {"scheduling", "federation", "guest_data", "realtime", "metering"}
    # Federation and scheduling have failure-load scenarios: their live tests apply
    # real concurrent/failure pressure even though their primary classification is
    # chaos/failover.
    all_targets = {target for scenario in scenarios for target in scenario.get("targets", [])}
    if not required_targets <= (load_targets | all_targets):
        fail(f"missing required load targets: {sorted(required_targets - (load_targets | all_targets))}")
    for scenario in scenarios:
        scenario_id = scenario["id"]
        command = scenario.get("command", "")
        if not command or not shlex.split(command):
            fail(f"{scenario_id}: missing command")
        thresholds = scenario.get("thresholds")
        if not isinstance(thresholds, dict) or not thresholds:
            fail(f"{scenario_id}: missing thresholds")
        if any(not isinstance(value, (int, float)) for value in thresholds.values()):
            fail(f"{scenario_id}: every threshold must be numeric")
        for evidence in scenario.get("evidence", []):
            if not (ROOT / evidence).is_file():
                fail(f"{scenario_id}: missing evidence {evidence}")
        env = scenario.get("required_env", [])
        if len(env) != len(set(env)) or any(not key.startswith("ASEMAN_") for key in env):
            fail(f"{scenario_id}: invalid required_env")
    print(f"operational scenario manifest holds ({len(scenarios)} scenarios)")


if __name__ == "__main__":
    if len(sys.argv) > 2 or (len(sys.argv) == 2 and sys.argv[1] != "--check"):
        fail("usage: check_operational_scenarios.py [--check]")
    main()
