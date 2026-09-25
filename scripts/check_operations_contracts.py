#!/usr/bin/env python3
"""Check A902 operation and recovery contracts against their Rust authority."""

from __future__ import annotations

import json
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def snake(name: str) -> str:
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()


def main() -> None:
    source = (ROOT / "crates/aseman-domain/src/operations.rs").read_text()
    match = re.search(r"pub enum OperationStep \{(?P<body>.*?)\n\}", source, re.S)
    if match is None:
        raise SystemExit("OperationStep enum is missing")
    rust_steps = {
        snake(name)
        for name in re.findall(r"^    ([A-Z][A-Za-z0-9]+),$", match.group("body"), re.M)
    }

    journal = json.loads(
        (ROOT / "contracts/operations/operation-journal.schema.json").read_text()
    )
    schema_steps = set(journal["$defs"]["step"]["enum"])
    if schema_steps != rust_steps:
        raise SystemExit(
            f"operation step drift: schema-only={sorted(schema_steps-rust_steps)}, "
            f"rust-only={sorted(rust_steps-schema_steps)}"
        )

    manifest = json.loads(
        (ROOT / "contracts/operations/backup-manifest.schema.json").read_text()
    )
    required = set(manifest["required"])
    recovery_fields = {
        "capsule_schema_versions",
        "provider_mappings",
        "module_versions",
        "artifacts",
        "signature",
    }
    if not recovery_fields <= required:
        raise SystemExit(f"backup manifest omits {sorted(recovery_fields-required)}")

    redaction = json.loads(
        (ROOT / "contracts/operations/support-bundle-redaction.json").read_text()
    )
    if not redaction.get("never_collect") or not redaction.get("value_patterns"):
        raise SystemExit("redaction contract must forbid sources and scrub residual values")
    print(f"operation contracts passed ({len(rust_steps)} journal steps)")


if __name__ == "__main__":
    main()
