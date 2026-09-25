#!/usr/bin/env python3
"""Generate the requirements traceability report (A1005, Phase 10).

Mechanically checks that every requirement in
``plan/migration/14-plan-integrity-and-traceability.md`` names a design authority, a
delivery phase, and an acceptance authority, then maps each requirement's delivery
phases to the status of the matching ``docs/migration/phase-*-gate.md``. A
requirement with a design gap, an unknown phase, or a missing acceptance authority is
reported as a violation; a requirement whose phases are all accepted is MET, one whose
phases are partial/in-progress is PARTIAL, and one whose phases are not accepted is
OPEN. The report is deterministic and ``--check`` fails on any drift or violation.

Requirement IDs, phase numbers, and design/acceptance citations are parsed from the
plan document, so the report cannot silently diverge from the traceability table.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

from inventory_common import retained_revision

ROOT = Path(__file__).resolve().parents[1]
PLAN = ROOT / "plan/migration/14-plan-integrity-and-traceability.md"
GATES = ROOT / "docs/migration"
JSON_PATH = ROOT / "docs/generated/requirements-traceability.json"
MARKDOWN_PATH = ROOT / "docs/generated/requirements-traceability.md"
GENERATOR = Path(__file__).name


def parse_rows() -> list[dict[str, str]]:
    """Parse the requirements traceability table."""
    text = PLAN.read_text(encoding="utf-8")
    section = text.split("## Requirements traceability", 1)[1]
    section = section.split("## Blocking decision gates", 1)[0]
    rows: list[dict[str, str]] = []
    for line in section.splitlines():
        if not line.startswith("| R"):
            continue
        cells = [cell.strip() for cell in line.strip("|").split("|")]
        if len(cells) != 5:
            raise ValueError(f"malformed traceability row: {line!r}")
        rows.append(
            {
                "id": cells[0],
                "requirement": cells[1],
                "design": cells[2],
                "phase": cells[3],
                "acceptance": cells[4],
            }
        )
    if not rows:
        raise ValueError("no requirements parsed from the traceability table")
    return rows


def parse_phase_numbers(phase: str) -> list[int]:
    """``1``, ``2, 9``, ``2-9``, ``all``, ``all, 9-10`` -> phase numbers."""
    numbers: list[int] = []
    for token in re.split(r"[,\s]+", phase):
        if not token:
            continue
        if token == "all":
            numbers.extend(range(0, 11))
        elif "-" in token:
            low, high = (int(part) for part in token.split("-"))
            numbers.extend(range(low, high + 1))
        else:
            numbers.append(int(token))
    return sorted(set(numbers))


def gate_status(phase: int) -> str:
    path = GATES / f"phase-{phase}-gate.md"
    if not path.exists():
        return "NO_GATE"
    text = path.read_text(encoding="utf-8")
    match = re.search(r"^status:\s*(\S+)", text, re.MULTILINE)
    return match.group(1) if match else "NO_STATUS"


def status_of(phase: int) -> str:
    return {
        "ACCEPTED": "MET",
        "PARTIAL": "PARTIAL",
        "IN_PROGRESS": "PARTIAL",
    }.get(gate_status(phase), "OPEN")


def build() -> dict[str, object]:
    rows = parse_rows()
    requirements = []
    violations: list[str] = []
    for row in rows:
        if not row["design"] or not row["acceptance"]:
            violations.append(f"{row['id']}: missing design or acceptance authority")
        phases = parse_phase_numbers(row["phase"])
        if not phases:
            violations.append(f"{row['id']}: no delivery phase")
        statuses = {phase: status_of(phase) for phase in phases}
        overall = "MET" if statuses and all(s == "MET" for s in statuses.values()) else (
            "PARTIAL" if any(s == "PARTIAL" for s in statuses.values()) else "OPEN"
        )
        requirements.append(
            {
                "id": row["id"],
                "requirement": row["requirement"],
                "design_authority": row["design"],
                "delivery_phase": row["phase"],
                "acceptance_authority": row["acceptance"],
                "phase_status": {str(phase): status for phase, status in statuses.items()},
                "status": overall,
            }
        )
    return {
        "_meta": {
            "artifact": "A1005",
            "status": "CURRENT",
            "lifecycle_status": "GENERATED",
            "source_of_truth": str(PLAN.relative_to(ROOT)),
            "last_verified_commit": retained_revision(ROOT, JSON_PATH),
            "verification": "python3 scripts/generate_requirements_traceability.py --check",
            "generator": GENERATOR,
        },
        "summary": {
            "requirements": len(requirements),
            "met": sum(r["status"] == "MET" for r in requirements),
            "partial": sum(r["status"] == "PARTIAL" for r in requirements),
            "open": sum(r["status"] == "OPEN" for r in requirements),
            "violations": len(violations),
        },
        "requirements": requirements,
        "violations": violations,
    }


def markdown(data: dict[str, object]) -> str:
    lines = [
        "---",
        "status: GENERATED",
        "owner: migration",
        "source_of_truth: plan/migration/14-plan-integrity-and-traceability.md via scripts/generate_requirements_traceability.py",
        "verification: python3 scripts/generate_requirements_traceability.py --check",
        "---",
        "",
        "# Requirements traceability (A1005)",
        "",
        "> Generated by `scripts/generate_requirements_traceability.py`. Do not edit by hand.",
        "",
        "Each requirement's delivery phases map to their phase-gate status. MET means every",
        "phase the requirement delivers in is accepted; PARTIAL means at least one is",
        "partial or in progress; OPEN means none is. A violation is a requirement that does",
        "not name a design authority, a delivery phase, or an acceptance authority.",
        "",
        f"- Requirements: {data['summary']['requirements']}",
        f"- MET: {data['summary']['met']}",
        f"- PARTIAL: {data['summary']['partial']}",
        f"- OPEN: {data['summary']['open']}",
        f"- Violations: {data['summary']['violations']}",
        "",
        "| Requirement | Delivery phase(s) | Phase status | Acceptance authority | Overall |",
        "|---|---|---|---|---|",
    ]
    for req in data["requirements"]:  # type: ignore[union-attr]
        phases = ", ".join(
            f"{phase}={status}" for phase, status in req["phase_status"].items()
        )
        lines.append(
            f"| {req['id']} {req['requirement']} | {req['delivery_phase']} | {phases} | {req['acceptance_authority']} | {req['status']} |"
        )
    if data["violations"]:
        lines.append("")
        lines.append("## Violations")
        for violation in data["violations"]:  # type: ignore[union-attr]
            lines.append(f"- {violation}")
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    data = build()
    json_out = json.dumps(data, indent=2, sort_keys=True) + "\n"
    md_out = markdown(data)
    if args.check:
        stale = (
            JSON_PATH.read_text(encoding="utf-8") != json_out
            or MARKDOWN_PATH.read_text(encoding="utf-8") != md_out
        )
        if data["violations"]:
            for violation in data["violations"]:
                print(f"traceability violation: {violation}", file=sys.stderr)
        if stale or data["violations"]:
            print("requirements traceability is stale or violated; regenerate without --check", file=sys.stderr)
            return 1
        print("requirements traceability is up to date")
        return 0
    JSON_PATH.write_text(json_out, encoding="utf-8")
    MARKDOWN_PATH.write_text(md_out, encoding="utf-8")
    print(f"wrote {JSON_PATH.name} and {MARKDOWN_PATH.name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())