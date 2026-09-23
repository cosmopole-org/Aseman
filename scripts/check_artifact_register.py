#!/usr/bin/env python3
"""Every required artifact has a recorded status (the register's completeness rule).

`plan/migration/16-required-artifacts-and-specification-backlog.md` names every artifact
the migration requires. `docs/migration/artifact-status.md` records what each one's state
actually is.

An artifact with no row is the worst case: not MISSING, which would at least be visible,
but *unmentioned*, which reads as done to anyone skimming. This check refuses that. It
found seven such artifacts on its first run, hidden behind a single lumped row.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REGISTER = ROOT / "plan/migration/16-required-artifacts-and-specification-backlog.md"
STATUS = ROOT / "docs/migration/artifact-status.md"

# A row must say which of these it is. "ACCEPTED (core)" and friends are not states:
# they let a partial delivery read as a complete one.
STATES = {"MISSING", "DRAFT", "ACCEPTED", "GENERATED", "VERIFIED", "RETIRED", "PARTIAL", "OPEN"}


def check() -> list[str]:
    problems: list[str] = []
    register = REGISTER.read_text(encoding="utf-8")
    status = STATUS.read_text(encoding="utf-8")

    required = re.findall(r"^\| (A\d{3,4}) \|", register, re.M)
    if not required:
        return ["the register lists no artifacts; this check is looking in the wrong place"]

    rows = dict(re.findall(r"^\|\s*(A\d{3,4})\s*\|\s*([A-Z()a-z ]+?)\s*\|", status, re.M))
    for artifact in required:
        if artifact not in rows:
            problems.append(f"{artifact} is required but has no status row")
            continue
        state = rows[artifact].strip().split()[0].upper()
        if state not in STATES:
            problems.append(
                f"{artifact} has state {rows[artifact].strip()!r}, which is not one of "
                + ", ".join(sorted(STATES))
            )
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="accepted for symmetry")
    parser.parse_args()
    problems = check()
    for problem in problems:
        print(f"artifact register: {problem}", file=sys.stderr)
    if problems:
        return 1
    print("every required artifact has a recorded status")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
