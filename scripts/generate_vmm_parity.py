#!/usr/bin/env python3
"""Validate the A505 native parity manifest and render the per-runtime matrix.

`contracts/vmm/native-parity.json` says where every native runtime operation (A006)
and every node-facing `IVmm` method goes when the embedded VMM is removed. This script
fails when an operation or method is unmapped, when a mapping names an A501 operation
that does not exist, or when an entry for a removed legacy method is not `deleted`. It
derives the capabilities the native backend must declare for each runtime from the
A006 overrides and each runtime's `vm.config.json`.
"""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "contracts/vmm/native-parity.json"
OPENAPI = ROOT / "contracts/vmm/openapi.json"
MATRIX = ROOT / "docs/generated/current-runtime-matrix.json"
IVMM = ROOT / "apps/aseman-node/src/models/ports/vmm.rs"
JSON_OUT = ROOT / "docs/generated/vmm-native-parity.json"
MD_OUT = ROOT / "docs/generated/vmm-native-parity.md"
GENERATOR = "scripts/generate_vmm_parity.py"
STATUSES = {"open", "verified", "deleted"}
CAPABILITIES = [
    "invocation", "long_running", "pause", "snapshot", "exec", "terminal",
    "http_ingress", "files", "build", "chain_transactions", "execution_proofs",
]


def ivmm_methods() -> list[str]:
    if not IVMM.exists():
        return []
    text = IVMM.read_text(encoding="utf-8")
    body = text[text.index("pub trait IVmm"):]
    return re.findall(r"^\s*fn (\w+)\s*\(", body, flags=re.MULTILINE)


def operation_ids(openapi: dict) -> set[str]:
    return {
        operation["operationId"]
        for item in openapi["paths"].values()
        for method, operation in item.items()
        if method != "parameters"
    }


def validate(manifest: dict, matrix: dict, openapi: dict, methods: list[str]) -> list[str]:
    errors = []
    ids = operation_ids(openapi)
    homes = set(manifest["homes"])
    runtime_ops = set(matrix["trait_operations"])
    for section, required, live in (
        ("runtime_operations", runtime_ops, runtime_ops),
        ("node_methods", set(methods), set(methods)),
    ):
        entries = manifest[section]
        for name in sorted(required - set(entries)):
            errors.append(f"{section}.{name}: unmapped")
        for name, entry in sorted(entries.items()):
            if entry["home"] not in homes:
                errors.append(f"{section}.{name}: unknown home {entry['home']}")
            target = entry.get("target")
            if entry["home"] == "api" and not target:
                errors.append(f"{section}.{name}: an api home needs a target")
            if target and target not in ids:
                errors.append(f"{section}.{name}: {target} is not an A501 operation")
            capability = entry.get("capability")
            if capability and capability not in CAPABILITIES:
                errors.append(f"{section}.{name}: unknown capability {capability}")
            if entry["status"] not in STATUSES:
                errors.append(f"{section}.{name}: unknown status {entry['status']}")
            if entry["status"] == "verified" and not entry.get("verification"):
                errors.append(f"{section}.{name}: verified without a verification")
            if name not in live and entry["status"] != "deleted":
                errors.append(f"{section}.{name}: gone from the legacy code but not `deleted`")
            if name in live and entry["status"] == "deleted":
                errors.append(f"{section}.{name}: `deleted` but still in the legacy code")
    rules = manifest["capability_rules"]
    if sorted(rules) != sorted(CAPABILITIES):
        errors.append("capability_rules must cover exactly the RuntimeCapabilities flags")
    return errors


def runtime_capabilities(manifest: dict, runtime: dict) -> dict:
    config = json.loads((ROOT / runtime["config"]).read_text(encoding="utf-8"))
    overrides = set(runtime["overrides"])
    result = {}
    for capability in CAPABILITIES:
        rule = manifest["capability_rules"][capability]
        supported = False
        if rule.get("never"):
            supported = False
        else:
            supported = bool(overrides & set(rule.get("override", [])))
            if "config" in rule:
                supported = supported or bool(config.get(rule["config"], False))
            if runtime["key"] in rule.get("exclude", {}):
                supported = False
        result[capability] = supported
    result["deploy"] = {
        "entity_file_name": config.get("entityFileName", "module.wasm"),
        "accepts_extra_files": bool(config.get("acceptsExtraFiles", False)),
        "build_on_deploy": bool(config.get("buildOnDeploy", False)),
    }
    return result


def render(manifest: dict, matrix: dict) -> tuple[dict, str]:
    runtimes = {
        runtime["key"]: runtime_capabilities(manifest, runtime)
        for runtime in matrix["runtimes"]
        if runtime["enabled_in_generated_aggregator"]
    }
    entries = [
        (section, name, entry)
        for section in ("runtime_operations", "node_methods")
        for name, entry in sorted(manifest[section].items())
    ]
    counts: dict[str, int] = {}
    for _, _, entry in entries:
        counts[entry["status"]] = counts.get(entry["status"], 0) + 1
    data = {
        "_meta": {
            "artifact": "A505",
            "generator": GENERATOR,
            "source_of_truth": "contracts/vmm/native-parity.json, A006, contracts/vmm/openapi.json",
            "verification": f"python3 {GENERATOR} --check",
        },
        "runtimes": runtimes,
        "status_counts": dict(sorted(counts.items())),
    }
    lines = [
        "---",
        "status: GENERATED",
        "owner: vmm",
        "source_of_truth: contracts/vmm/native-parity.json",
        f"verification: python3 {GENERATOR} --check",
        "---",
        "",
        "# A505: native VMM parity matrix",
        "",
        f"Generated by `{GENERATOR}`. Do not edit.",
        "",
        "## Capabilities the native backend declares",
        "",
        "Derived from the A006 overrides and each runtime's `vm.config.json`. An operation",
        "a runtime does not support is refused with `unsupported_operation` (A501).",
        "",
        "| Runtime | " + " | ".join(CAPABILITIES) + " | Entity file | Build on deploy |",
        "|---|" + "---|" * (len(CAPABILITIES) + 2),
    ]
    for key, capabilities in runtimes.items():
        cells = ["yes" if capabilities[c] else "no" for c in CAPABILITIES]
        deploy = capabilities["deploy"]
        lines.append(
            f"| {key} | " + " | ".join(cells)
            + f" | `{deploy['entity_file_name']}` | {'yes' if deploy['build_on_deploy'] else 'no'} |"
        )
    status_line = ", ".join(f"{count} {status}" for status, count in sorted(counts.items()))
    lines += ["", "## Where each legacy operation goes", "", f"Status: {status_line}.", ""]
    for section, title in (("runtime_operations", "Runtime operations (A006)"), ("node_methods", "Node-facing `IVmm` methods")):
        lines += [f"### {title}", "", "| Operation | Home | Target | Status | Note |", "|---|---|---|---|---|"]
        for name, entry in sorted(manifest[section].items()):
            target = f"`{entry['target']}`" if entry.get("target") else ""
            note = entry["note"]
            if entry.get("verification"):
                note = f"{note} Verified by `{entry['verification']}`.".strip()
            lines.append(f"| `{name}` | {entry['home']} | {target} | {entry['status']} | {note} |")
        lines.append("")
    return data, "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    matrix = json.loads(MATRIX.read_text(encoding="utf-8"))
    openapi = json.loads(OPENAPI.read_text(encoding="utf-8"))
    errors = validate(manifest, matrix, openapi, ivmm_methods())
    if errors:
        for error in errors:
            print(f"error: {error}")
        return 1
    data, markdown = render(manifest, matrix)
    json_text = json.dumps(data, indent=2) + "\n"
    if args.check:
        stale = [
            str(path.relative_to(ROOT))
            for path, text in ((JSON_OUT, json_text), (MD_OUT, markdown))
            if not path.exists() or path.read_text(encoding="utf-8") != text
        ]
        if stale:
            print("stale: " + ", ".join(stale))
            return 1
        return 0
    JSON_OUT.write_text(json_text, encoding="utf-8")
    MD_OUT.write_text(markdown, encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
