#!/usr/bin/env python3
"""Run the A1002 operational scenarios and write one report per scenario.

Each report follows the manifest's `report_schema` (scenario, started_at,
duration_seconds, environment, measurements, thresholds, passed). A scenario whose
`required_env` is not set is reported as skipped (`passed: null`), never as passed.
The numeric thresholds are enforced by each scenario's own command; the runner records
its outcome, cargo test counts, and the tail of its output as evidence.

Environment values are never written to a report: only which required variables were
present, because they carry database URLs and credentials.

Usage:
  run_operational_scenarios.py --out DIR [--only ID ...]
  run_operational_scenarios.py --list
"""

from __future__ import annotations

import argparse
import datetime
import json
import os
import platform
import re
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "contracts/testing/operational-scenarios-v1.json"
TEST_RESULT = re.compile(r"test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored")


def commit() -> str:
    result = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True, check=False
    )
    dirty = subprocess.run(
        ["git", "status", "--porcelain"], cwd=ROOT, capture_output=True, text=True, check=False
    )
    return result.stdout.strip() + ("+dirty" if dirty.stdout.strip() else "")


def run(scenario: dict, source_commit: str) -> dict:
    required = scenario.get("required_env", [])
    missing = [key for key in required if not os.environ.get(key)]
    started = datetime.datetime.now(datetime.timezone.utc)
    report = {
        "scenario": scenario["id"],
        "kind": scenario["kind"],
        "command": scenario["command"],
        "started_at": started.isoformat(),
        "environment": {
            "commit": source_commit,
            "host": platform.node(),
            "platform": platform.platform(),
            "required_env_present": sorted(set(required) - set(missing)),
            "required_env_missing": missing,
        },
        "thresholds": scenario["thresholds"],
    }
    if missing:
        report.update(duration_seconds=0, measurements={}, passed=None)
        report["skipped"] = f"required environment not set: {', '.join(missing)}"
        return report
    clock = time.monotonic()
    completed = subprocess.run(
        scenario["command"],
        cwd=ROOT,
        shell=True,
        capture_output=True,
        text=True,
        check=False,
    )
    output = completed.stdout + completed.stderr
    totals = {"passed": 0, "failed": 0, "ignored": 0}
    for _, passed, failed, ignored in TEST_RESULT.findall(output):
        totals["passed"] += int(passed)
        totals["failed"] += int(failed)
        totals["ignored"] += int(ignored)
    report.update(
        duration_seconds=round(time.monotonic() - clock, 3),
        measurements={
            "exit_code": completed.returncode,
            "tests": totals,
            "output_tail": output.splitlines()[-40:],
        },
        # A command that ran no test at all proves nothing.
        passed=completed.returncode == 0
        and (totals["passed"] > 0 or not scenario["command"].startswith("cargo test")),
    )
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--out", type=Path)
    parser.add_argument("--only", action="append", default=[])
    parser.add_argument("--list", action="store_true")
    arguments = parser.parse_args()
    scenarios = json.loads(MANIFEST.read_text(encoding="utf-8"))["scenarios"]
    if arguments.list:
        for scenario in scenarios:
            print(f"{scenario['id']}\t{scenario['kind']}\t{','.join(scenario.get('required_env', []))}")
        return 0
    if arguments.out is None:
        parser.error("--out DIR is required")
    unknown = set(arguments.only) - {scenario["id"] for scenario in scenarios}
    if unknown:
        parser.error(f"unknown scenario(s): {', '.join(sorted(unknown))}")
    arguments.out.mkdir(parents=True, exist_ok=True)
    source_commit = commit()
    summary = []
    for scenario in scenarios:
        if arguments.only and scenario["id"] not in arguments.only:
            continue
        report = run(scenario, source_commit)
        (arguments.out / f"{scenario['id']}.json").write_text(
            json.dumps(report, indent=2) + "\n", encoding="utf-8"
        )
        state = {True: "PASS", False: "FAIL", None: "SKIP"}[report["passed"]]
        summary.append({"scenario": scenario["id"], "result": state, "seconds": report["duration_seconds"]})
        print(f"{state}\t{scenario['id']}\t{report['duration_seconds']}s", flush=True)
    (arguments.out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    return 1 if any(item["result"] == "FAIL" for item in summary) else 0


if __name__ == "__main__":
    sys.exit(main())
