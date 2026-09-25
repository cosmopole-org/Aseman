#!/usr/bin/env python3
"""Validate the versioned cold-start agent evaluation catalog."""

from __future__ import annotations

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CATALOG = ROOT / "evals" / "agent" / "cases.json"


def main() -> None:
    data = json.loads(CATALOG.read_text())
    assert data["version"] == 1, "unsupported agent-evaluation catalog version"
    cases = data["cases"]
    ids = [case["id"] for case in cases]
    assert len(ids) == len(set(ids)), "agent-evaluation IDs must be unique"
    assert cases, "at least one agent evaluation is required"
    for case in cases:
        assert case["question"].strip(), f"{case['id']}: question is empty"
        assert case["verification"].strip(), f"{case['id']}: verification is empty"
        assert case["authorities"], f"{case['id']}: no authority is named"
        for authority in case["authorities"]:
            assert (ROOT / authority).exists(), f"{case['id']}: missing {authority}"
    print(f"agent comprehension catalog holds ({len(cases)} cases)")


if __name__ == "__main__":
    main()
