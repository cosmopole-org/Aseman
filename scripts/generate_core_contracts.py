#!/usr/bin/env python3
"""Generate Phase 1 config schema, alias map, and domain/port catalogs."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any

from inventory_common import retained_revision


ROOT = Path(__file__).resolve().parents[1]
CONFIG_INPUT = ROOT / "docs/generated/current-configuration.json"
SCHEMA = ROOT / "contracts/config/aseman-config.schema.json"
ALIASES = ROOT / "contracts/config/legacy-aliases.json"
DOMAIN_DOC = ROOT / "docs/generated/domain-catalog.md"
PORT_DOC = ROOT / "docs/generated/port-catalog.md"
GENERATOR = "scripts/generate_phase1_contracts.py"
OVERRIDES = {
    "OWNER_ID": "ASEMAN_NODE_ID",
    "OWNER_PRIVATE_KEY": "ASEMAN_NODE_PRIVATE_KEY_SECRET",
    "DATABASE_URL_SECRET": "ASEMAN_DATABASE_URL_SECRET",
    "VMM_ENDPOINT": "ASEMAN_VMM_ENDPOINT",
    "CLIENT_TCP_API_PORT": "ASEMAN_LEGACY_TCP_PORT",
    "CLIENT_WS_API_PORT": "ASEMAN_LEGACY_WS_PORT",
    "FEDERATION_API_PORT": "ASEMAN_LEGACY_FEDERATION_PORT",
    "BLOCKCHAIN_API_PORT": "ASEMAN_LEGACY_CONSENSUS_PORT",
    "CASPAR_STORAGE_PORT": "ASEMAN_PUBLIC_STORAGE_PORT",
}


def canonical(key: str) -> str:
    if key in OVERRIDES:
        return OVERRIDES[key]
    if key.startswith("ASEMAN_"):
        return key
    if key.startswith("CASPAR_"):
        return "ASEMAN_" + key.removeprefix("CASPAR_")
    return "ASEMAN_LEGACY_" + key


def property_schema(item: dict[str, Any]) -> dict[str, Any]:
    key = item["key"]
    sample = item.get("sample_value")
    upper = key.upper()
    if upper.endswith(("_PORT", "_SECONDS", "_MS", "_LIMIT", "_SIZE", "_COUNT")):
        value: dict[str, Any] = {"type": "integer", "minimum": 0}
        if upper.endswith("_PORT"):
            value["maximum"] = 65535
    elif upper.startswith(("ENABLE_", "DISABLE_")) or upper.endswith(("_ENABLED", "_DISABLED")):
        value = {"type": "boolean"}
    else:
        value = {"type": "string"}
    value["description"] = f"Typed replacement for legacy {key} ({item['category']})."
    value["x-legacy-aliases"] = [key] if canonical(key) != key else []
    if any(word in upper for word in ("SECRET", "PRIVATE_KEY", "PASSWORD", "TOKEN")):
        value["writeOnly"] = True
        value["x-secret-reference"] = True
    elif sample not in (None, ""):
        value["x-observed-sample"] = sample
    return value


def config_contract() -> tuple[dict[str, Any], dict[str, Any]]:
    current = json.loads(CONFIG_INPUT.read_text(encoding="utf-8"))
    revision = retained_revision(ROOT, ALIASES)
    alias_rows = []
    properties: dict[str, Any] = {}
    collisions: dict[str, list[str]] = {}
    for item in current["keys"]:
        target = canonical(item["key"])
        collisions.setdefault(target, []).append(item["key"])
        properties.setdefault(target, property_schema(item))
        alias_rows.append(
            {
                "legacy": item["key"],
                "canonical": target,
                "category": item["category"],
                "conflict_policy": "fail",
                "expiry": "ADR-0004 window after canonical replacement",
                "occurrences": item["occurrences"],
            }
        )
    duplicate_targets = {key: values for key, values in collisions.items() if len(values) > 1}
    schema = {
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://aseman.example/contracts/config/aseman-config.schema.json",
        "title": "AsemanConfig",
        "description": "Canonical typed configuration keys. Secret fields contain secret references, never secret values in persisted config.",
        "type": "object",
        "additionalProperties": False,
        "properties": dict(sorted(properties.items())),
        "x-generator": GENERATOR,
        "x-last-verified-commit": revision,
    }
    aliases = {
        "_meta": {
            "artifact": "A103",
            "status": "ACCEPTED",
            "last_verified_commit": revision,
            "generator": GENERATOR,
            "legacy_key_count": len(alias_rows),
        },
        "aliases": alias_rows,
        "canonical_collisions": duplicate_targets,
        "rules": [
            "Canonical and legacy values supplied together fail closed, even when textually equal.",
            "Legacy usage emits a redaction-safe warning and counter.",
            "Aliases are removed only under ADR 0004 and their removal-ledger row.",
            "Unknown keys fail schema validation.",
        ],
    }
    return schema, aliases


def public_items(path: Path, kind: str) -> list[str]:
    text = path.read_text(encoding="utf-8")
    if kind == "domain":
        pattern = re.compile(r"^pub\s+(?:struct|enum)\s+([A-Za-z0-9_]+)", re.MULTILINE)
    else:
        pattern = re.compile(r"^pub\s+trait\s+([A-Za-z0-9_]+)", re.MULTILINE)
    return sorted(set(pattern.findall(text)))


def catalog(title: str, crate: str, values: list[str], artifact: str, revision: str) -> str:
    label = "Domain type/state machine" if artifact == "A104" else "Behavioral port"
    lines = [
        "---", "status: GENERATED", "owner: architecture/phase-1",
        f"source_of_truth: crates/{crate}/src/lib.rs", f"last_verified_commit: {revision[:12]}",
        f"verification: python3 {GENERATOR} --check", "---", "", f"# {title}", "",
        f"| {label} | Owning crate |", "|---|---|",
    ]
    lines.extend(f"| `{value}` | `{crate}` |" for value in values)
    lines += ["", "This catalog is generated from public declarations. Semantic guarantees remain", "in the source documentation, accepted ADRs, and conformance tests.", ""]
    return "\n".join(lines)


def write_or_check(path: Path, value: str, check: bool) -> bool:
    if check:
        return path.exists() and path.read_text(encoding="utf-8") == value
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(value, encoding="utf-8")
    return True


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    schema, aliases = config_contract()
    revision = aliases["_meta"]["last_verified_commit"]
    outputs = {
        SCHEMA: json.dumps(schema, indent=2, sort_keys=True) + "\n",
        ALIASES: json.dumps(aliases, indent=2, sort_keys=True) + "\n",
        DOMAIN_DOC: catalog("Domain catalog", "aseman-domain", public_items(ROOT / "crates/aseman-domain/src/lib.rs", "domain"), "A104", revision),
        PORT_DOC: catalog("Port catalog", "aseman-ports", public_items(ROOT / "crates/aseman-ports/src/lib.rs", "port"), "A105", revision),
    }
    ok = all(write_or_check(path, value, args.check) for path, value in outputs.items())
    if args.check and not ok:
        print("Phase 1 generated contracts are stale", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
