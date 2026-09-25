#!/usr/bin/env python3
"""Generate the Phase 0 shell action call graph (A005)."""

from __future__ import annotations

import argparse
import json
import functools
import re
import sys
from collections import Counter
from pathlib import Path
from typing import Any

from inventory_common import retained_revision


ROOT = Path(__file__).resolve().parents[1]
JSON_PATH = ROOT / "docs/generated/current-call-graph.json"
MARKDOWN_PATH = ROOT / "docs/migration/current-call-graph.md"
GENERATOR = "scripts/generate_current_call_graph.py"
COMMIT = retained_revision(ROOT, JSON_PATH)

TRX_METHODS = (
    "del_key|get_by_prefix|has_obj|get_index|put_index|del_index|has_index|"
    "get_column|get_links_list|search_link_vals_list|search_link_keys_list_by_prefix|"
    "get_obj_list|get_link|put_link|put_bytes|get_bytes|put_string|get_string|"
    "get_obj|put_obj|put_json|del_json|get_json|commit|discard"
)



@functools.lru_cache(maxsize=1)
def vmm_client_methods() -> frozenset:
    """The node's VMM client surface, read from `RemoteWorkloads` itself.

    The call graph names the A501 operations an action performs. Reading the surface
    from the client keeps the inventory honest when the client gains or loses one,
    instead of pinning a list that silently goes stale.
    """
    source = (ROOT / "apps/aseman-node/src/shell/workloads.rs").read_text(encoding="utf-8")
    blocks = [
        source[start : source.find("\n}\n", start)]
        for start in (
            match.start()
            for match in re.finditer(r"\nimpl RemoteWorkloads \{", source)
        )
    ]
    if not blocks:
        raise SystemExit("RemoteWorkloads is gone: update the call graph generator")
    return frozenset(
        name
        for block in blocks
        for name in re.findall(
            r"\bpub(?:\(crate\))? fn ([a-z][a-zA-Z0-9_]*)\s*[(<]", block
        )
    )

def rel(path: Path) -> str:
    return path.relative_to(ROOT).as_posix()


def line_number(value: str, offset: int) -> int:
    return value.count("\n", 0, offset) + 1


def matching_block(value: str, open_brace: int) -> tuple[str, int]:
    depth = 0
    quote: str | None = None
    escaped = False
    line_comment = False
    block_comment = 0
    index = open_brace
    while index < len(value):
        char = value[index]
        nxt = value[index + 1] if index + 1 < len(value) else ""
        if line_comment:
            if char == "\n":
                line_comment = False
        elif block_comment:
            if char == "/" and nxt == "*":
                block_comment += 1
                index += 1
            elif char == "*" and nxt == "/":
                block_comment -= 1
                index += 1
        elif quote:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                quote = None
        elif char == "/" and nxt == "/":
            line_comment = True
            index += 1
        elif char == "/" and nxt == "*":
            block_comment = 1
            index += 1
        elif char in {'"', "'"}:
            quote = char
        elif char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return value[open_brace : index + 1], index + 1
        index += 1
    raise ValueError("unbalanced Rust block")


def normalized_guard(body: str, route: str) -> str:
    route_at = body.find(f'"{route}"')
    if route_at < 0:
        return "unresolved"
    tail = body[route_at + len(route) + 2 :]
    move_at = tail.find("move |")
    if move_at < 0:
        return "unresolved"
    guard = tail[:move_at].strip().strip(",").strip()
    return " ".join(guard.split()) or "unresolved"


def action_rows() -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    route_pattern = re.compile(
        r'build_secure_action::<\s*([^,>]+)\s*,\s*_\s*>\(\s*[^,]+,\s*"([^\"]+)"',
        re.S,
    )
    function_pattern = re.compile(
        r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?fn\s+([A-Za-z0-9_]+)\s*\("
    )
    # Action families are progressively partitioned into owned submodules.
    # The call graph follows the public registrations, not the old flat layout.
    for path in sorted((ROOT / "apps/aseman-node/src/shell/api/actions").rglob("*.rs")):
        value = path.read_text(encoding="utf-8")
        for function in function_pattern.finditer(value):
            open_brace = value.find("{", function.end())
            if open_brace < 0:
                continue
            body, _ = matching_block(value, open_brace)
            route = route_pattern.search(body)
            if not route:
                continue
            request_type, action_path = route.groups()
            transaction_operations = sorted(
                set(re.findall(rf"\.(?:{TRX_METHODS})\s*\(", body))
            )
            transaction_operations = [item[1:].split("(", 1)[0].strip() for item in transaction_operations]
            services = sorted(
                set(re.findall(r"\.tools\(\)\.([a-zA-Z0-9_]+)\(\)", body))
            )
            if ".globe()" in body or "send_base_request_on_chain" in body:
                services.append("globe/chain")
            if "modify_state" in body:
                services.append("state")
            models = sorted(
                set(
                    re.findall(
                        r"\b(Creature|Program|Store|Session|Entity|File|Chain|ChainShard)(?:::|\s*\{)",
                        body,
                    )
                )
            )
            # Since P5-06 an action reaches a VM only through the node's VMM client
            # (`shell::workloads::remote()`), never through an in-process runtime.
            vmm_calls = []
            if "shell::workloads::remote()" in body:
                services.append("vmm")
                vmm_calls = sorted(
                    name
                    for name in vmm_client_methods()
                    if re.search(rf"\.{name}\s*\(", body)
                )
            rows.append(
                {
                    "path": action_path,
                    "handler": f"{rel(path)}::{function.group(1)}",
                    "source": f"{rel(path)}:{line_number(value, function.start())}",
                    "request_type": request_type.strip(),
                    "guard_expression": normalized_guard(body, action_path),
                    "transaction_operations": transaction_operations,
                    "services": sorted(set(services)),
                    "models": models,
                    "vmm_operations": vmm_calls,
                    "routes_via_chain": "send_base_request_on_chain" in body,
                    "spawns_background_work": "thread::spawn" in body or "async_once" in body,
                }
            )
    return sorted(rows, key=lambda row: row["path"])


def inventory() -> dict[str, Any]:
    rows = action_rows()
    guard_counts = Counter(row["guard_expression"] for row in rows)
    service_counts = Counter(service for row in rows for service in row["services"])
    return {
        "_meta": {
            "artifact": "A005",
            "status": "CURRENT",
            "lifecycle_status": "GENERATED",
            "source_of_truth": "registered shell action builder functions",
            "last_verified_commit": COMMIT,
            "verification": f"python3 {GENERATOR} --check",
            "generator": GENERATOR,
        },
        "shared_dispatch": {
            "registration": "apps/aseman-node/src/shell/api/main.rs::plug_all",
            "wrapper": "apps/aseman-node/src/shell/api/actions/util.rs::build_secure_action",
            "authorization": "apps/aseman-node/src/core/actor/model/secured/guard.rs::Guard",
            "local_execution": "ICore::modify_state_securly",
            "chain_routing": "SecureAction::dispatch_via_chain",
            "federation_routing": "SecureAction::dispatch_via_federation",
            "note": "Guard authentication precedes local action execution, but action-specific authorization remains distributed through handlers and services.",
        },
        "summary": {
            "action_count": len(rows),
            "unique_guard_expressions": len(guard_counts),
            "actions_with_vmm_operations": sum(bool(row["vmm_operations"]) or "vmm" in row["services"] for row in rows),
            "actions_with_storage_operations": sum(bool(row["transaction_operations"]) or "storage" in row["services"] for row in rows),
            "actions_spawning_background_work": sum(row["spawns_background_work"] for row in rows),
            "guard_expression_counts": dict(sorted(guard_counts.items())),
            "service_reference_counts": dict(sorted(service_counts.items())),
        },
        "actions": rows,
        "limitations": [
            "Indirect helper calls can hide additional storage, VMM, policy, finance, and signalling effects.",
            "A Guard expression records entry authentication/membership only; it is not proof of complete action authorization.",
            "Dynamic dispatch through ICore, IVmm, federation, and chain callbacks requires characterization tests before extraction.",
            "Guest host operations remain a separate surface recorded in A002 and need their own policy call-path audit.",
        ],
    }


def markdown(data: dict[str, Any]) -> str:
    summary = data["summary"]
    lines = [
        "---",
        "status: CURRENT",
        "owner: migration/P0-01",
        "source_of_truth: registered shell action builder functions",
        f"last_verified_commit: {COMMIT}",
        f"verification: python3 {GENERATOR} --check",
        "---",
        "",
        "# Current action call graph",
        "",
        f"> Generated by `{GENERATOR}`. Do not edit by hand.",
        "",
        "This A005 artifact maps each registered shell action to its handler, entry guard,",
        "transaction operations, model owners, service references, and direct VMM operations.",
        "The complete machine-readable graph is in `docs/generated/current-call-graph.json`.",
        "",
        "## Shared dispatch path",
        "",
        "```text",
        "TCP / WebSocket / chain / federation packet",
        "  -> registered SecureAction",
        "  -> typed JSON parser",
        "  -> Guard signature/identity and optional store-membership check",
        "  -> local state modification OR chain/federation forwarding",
        "  -> action handler",
        "  -> transaction/model/service/VMM calls",
        "```",
        "",
        f"Registered actions: {summary['action_count']}; distinct guard expressions: {summary['unique_guard_expressions']}.",
        "",
        "## Actions",
        "",
        "| Path | Handler | Guard | Storage operations | Services | Direct VMM operations |",
        "|---|---|---|---|---|---|",
    ]
    for row in data["actions"]:
        storage = ", ".join(f"`{item}`" for item in row["transaction_operations"]) or "—"
        services = ", ".join(f"`{item}`" for item in row["services"]) or "—"
        vmm = ", ".join(f"`{item}`" for item in row["vmm_operations"]) or "—"
        lines.append(
            f"| `{row['path']}` | `{row['handler']}` | `{row['guard_expression']}` | {storage} | {services} | {vmm} |"
        )
    lines += ["", "## Limitations and required follow-up", ""]
    lines.extend(f"- {item}" for item in data["limitations"])
    lines += [
        "- This graph is characterization input, not evidence that the current authorization model satisfies the target policy invariant.",
        "",
    ]
    return "\n".join(lines)


def outputs() -> dict[Path, str]:
    data = inventory()
    return {
        JSON_PATH: json.dumps(data, indent=2, sort_keys=True) + "\n",
        MARKDOWN_PATH: markdown(data),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    records = outputs()
    if args.check:
        stale = [path for path, value in records.items() if not path.exists() or path.read_text() != value]
        if stale:
            for path in stale:
                print(f"stale: {rel(path)}", file=sys.stderr)
            return 1
        print("current call graph is up to date")
        return 0
    for path, value in records.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(value, encoding="utf-8")
        print(f"wrote {rel(path)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
