#!/usr/bin/env python3
"""Validate the A402 action registry and render its generated reference.

`contracts/security/actions.json` is the maintained source of truth. This script fails
when the registry is inconsistent, when an inventoried surface (A002 routes and guest
operations, the P2 module admin API) maps to no action, or when an action claims a
surface that no longer exists. Unknown actions deny (ADR 0008), so an unmapped surface
would be unauthorizable.
"""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REGISTRY = ROOT / "contracts/security/actions.json"
ROUTES = ROOT / "docs/generated/current-routes.json"
MODULE_ADMIN = ROOT / "contracts/module/admin.openapi.yaml"
MD_OUT = ROOT / "docs/generated/security-action-registry.md"
GENERATOR = "scripts/generate_security_registry.py"
ACTION_ID = re.compile(r"^[a-z_]+(\.[a-z_]+)+$")
CLASSES = {"read", "write", "security", "financial", "administrative"}


def inventoried_surfaces() -> set[str]:
    routes = json.loads(ROUTES.read_text(encoding="utf-8"))
    surfaces = {f"signed-shell-action {a['path']}" for a in routes["signed_shell_actions"]}
    surfaces |= {f"{g['gateway']} {g['operation']}" for g in routes["guest_operations"]}
    surfaces |= {f"{h['surface']} {h['method']} {h['path']}" for h in routes["http_routes"]}
    operations = re.findall(r"operationId:\s*(\S+)", MODULE_ADMIN.read_text(encoding="utf-8"))
    surfaces |= {f"module-admin {operation}" for operation in operations}
    return surfaces


def validate(registry: dict) -> list[str]:
    errors = []
    subjects = set(registry["subjects"])
    resources = set(registry["resources"])
    conditions = set(registry["conditions"])
    guards = set(registry["legacy_guards"])
    seen_ids: set[str] = set()
    claimed: dict[str, str] = {}
    for action in registry["actions"]:
        action_id = action["id"]
        if not ACTION_ID.match(action_id):
            errors.append(f"{action_id}: invalid action id")
        if action_id in seen_ids:
            errors.append(f"{action_id}: duplicate action id")
        seen_ids.add(action_id)
        if action["resource"] not in resources:
            errors.append(f"{action_id}: unknown resource {action['resource']}")
        if action["class"] not in CLASSES:
            errors.append(f"{action_id}: unknown class {action['class']}")
        if not action["subjects"] or not set(action["subjects"]) <= subjects:
            errors.append(f"{action_id}: invalid subjects {action['subjects']}")
        rule = action["rule"]
        if not rule or not set(rule) <= conditions:
            errors.append(f"{action_id}: invalid rule {rule}")
        if "never" in rule and (rule != ["never"] or "removal" not in action):
            errors.append(f"{action_id}: `never` stands alone and names its removal")
        if "public" in rule and action["class"] in {"security", "financial", "administrative"} and action_id not in {
            "identity.session.create",
            "identity.challenge.issue",
        }:
            errors.append(f"{action_id}: a {action['class']} action cannot be public")
        if action["legacy_guard"] not in guards:
            errors.append(f"{action_id}: unknown legacy guard {action['legacy_guard']}")
        for surface in action["surfaces"]:
            if surface in claimed:
                errors.append(f"{surface}: claimed by {claimed[surface]} and {action_id}")
            claimed[surface] = action_id
    inventory = inventoried_surfaces()
    for surface in sorted(inventory - set(claimed)):
        errors.append(f"unmapped surface: {surface}")
    for surface in sorted(set(claimed) - inventory):
        errors.append(f"stale surface: {surface} ({claimed[surface]})")
    return errors


def markdown(registry: dict) -> str:
    actions = registry["actions"]
    lines = [
        "---",
        "status: GENERATED",
        "owner: security/authority",
        f"source_of_truth: contracts/security/actions.json (registry {registry['registry_version']})",
        f"verification: python3 {GENERATOR} --check",
        "---",
        "",
        "# A402 action registry",
        "",
        f"{len(actions)} actions over {len(registry['resources'])} resource types. Every inventoried "
        "surface maps to exactly one action; unknown actions deny (ADR 0008).",
        "",
        "## Conditions",
        "",
        "| Condition | Meaning |",
        "|---|---|",
    ]
    lines += [f"| `{name}` | {text} |" for name, text in registry["conditions"].items()]
    lines += ["", "## Actions", "", "| Action | Resource | Class | Subjects | Rule | Legacy guard | Surfaces |",
              "|---|---|---|---|---|---|---|"]
    for action in actions:
        rule = " or ".join(f"`{condition}`" for condition in action["rule"])
        if "removal" in action:
            rule += f" (removed: {action['removal']})"
        lines.append(
            f"| `{action['id']}` | {action['resource']} | {action['class']} | "
            f"{', '.join(action['subjects'])} | {rule} | {action['legacy_guard']} | {len(action['surfaces'])} |"
        )
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    registry = json.loads(REGISTRY.read_text(encoding="utf-8"))
    errors = validate(registry)
    if errors:
        for error in errors:
            print(error)
        return 1
    content = markdown(registry)
    if args.check:
        if not MD_OUT.exists() or MD_OUT.read_text(encoding="utf-8") != content:
            print(f"stale: {MD_OUT.relative_to(ROOT)}")
            return 1
        return 0
    MD_OUT.write_text(content, encoding="utf-8")
    print(f"wrote {MD_OUT.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
