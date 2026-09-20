"""Shared helpers for reproducible Phase 0 inventory generators."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path


def retained_revision(root: Path, primary_artifact: Path) -> str:
    """Keep the revision already recorded by a generated baseline.

    A generated file necessarily records the commit *before* that file is
    committed.  Reusing that value keeps ``--check`` stable after documentation
    commits.  Deleting the generated artifact and regenerating it intentionally
    starts a new baseline at the current revision.
    """

    if primary_artifact.exists():
        try:
            value = json.loads(primary_artifact.read_text(encoding="utf-8"))
            revision = value.get("_meta", {}).get("last_verified_commit")
            if isinstance(revision, str) and revision:
                return revision
        except (OSError, json.JSONDecodeError):
            pass

    return subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=root,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    ).stdout.strip()
