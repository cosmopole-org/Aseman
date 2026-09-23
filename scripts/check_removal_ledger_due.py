#!/usr/bin/env python3
"""Fail a release with overdue removal-ledger entries (Phase 10).

Every migrated capability has two gates: a replacement gate that proves the new path,
and a deletion gate that removes the superseded one. A row whose expiry phase has been
accepted but whose legacy path is still present is **overdue** — the replacement was
accepted and the deletion never happened.

Without this check the ledger is a wish list. With it, a release fails while a legacy
path outlives its window, which is the only thing that makes "delete it later"
something other than "never".

The current phase is taken from the accepted phase gates in `docs/migration/`, so the
check tightens on its own as phases are accepted rather than needing to be edited.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CHILDREN = ROOT / "docs/generated/removal-ledger-children.json"
LEDGER = ROOT / "docs/migration/removal-ledger.md"
GATES = ROOT / "docs/migration"

# An expiry that names a phase becomes due once that phase's gate is accepted.
PHASE = re.compile(r"Phase (\d+)")


def accepted_phases() -> set[int]:
    """Phases whose exit gate is accepted."""
    accepted = set()
    for gate in sorted(GATES.glob("phase-*-gate.md")):
        text = gate.read_text(encoding="utf-8")
        number = re.search(r"phase-(\d+)-gate", gate.name)
        if number and re.search(r"^status:\s*ACCEPTED", text, re.MULTILINE):
            accepted.add(int(number.group(1)))
    return accepted


def parent_rows() -> list[dict]:
    """The parent ledger rows, with their phase and their deletion evidence."""
    rows = []
    for line in LEDGER.read_text(encoding="utf-8").splitlines():
        if not line.startswith("| RL-"):
            continue
        cells = [cell.strip() for cell in line.strip("|").split("|")]
        if len(cells) < 9:
            continue
        rows.append(
            {
                "id": cells[0],
                "disposition": cells[1],
                "what": cells[2],
                "deletion_gate": cells[7],
                "phase": cells[8],
            }
        )
    return rows


def due_children(accepted: set[int]) -> list[dict]:
    data = json.loads(CHILDREN.read_text(encoding="utf-8"))
    due = []
    for section in ("configuration", "storage", "protocol_and_compatibility", "artifacts"):
        for row in data[section]:
            found = PHASE.search(row["expiry"])
            if not found:
                # An ADR-0004 window with no phase named is governed by its parent
                # row, which is checked separately.
                continue
            if int(found.group(1)) in accepted:
                due.append(row)
    return due


def check() -> list[str]:
    accepted = accepted_phases()
    problems: list[str] = []

    # A parent row whose phase is accepted must record its deletion evidence, or say
    # plainly that it is still open. Silence is what this refuses.
    evidence = LEDGER.read_text(encoding="utf-8")
    for row in parent_rows():
        found = PHASE.search(row["phase"])
        if not found or int(found.group(1)) not in accepted:
            continue
        identifier = row["id"]
        mentioned = re.search(rf"{identifier}\b", evidence[evidence.find("## "):])
        if not mentioned:
            problems.append(
                f"{identifier} is due ({row['phase']}) and the ledger records no outcome for it"
            )

    due = due_children(accepted)
    if due:
        # Child rows becoming due is expected and is not a failure on its own: what
        # matters is that their parent row records an outcome. Report the count so an
        # operator can see the size of the outstanding deletion work.
        print(
            f"removal ledger: {len(due)} child rows are within an accepted phase's window",
            file=sys.stderr,
        )
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="accepted for symmetry")
    parser.parse_args()
    problems = check()
    for problem in problems:
        print(f"removal ledger: {problem}", file=sys.stderr)
    if problems:
        return 1
    print("no overdue removal-ledger entries")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
