#!/usr/bin/env python3
"""Generate Phase 0 inventories for current interfaces and configuration.

The legacy project does not yet have source registries for these surfaces.  The
extractors below are deliberately narrow and preserve source locations so every
generated row can be reviewed before it becomes an accepted migration input.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any

from inventory_common import retained_revision


ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "docs/generated"
GENERATOR = "scripts/generate_current_surface_inventories.py"
COMMIT = retained_revision(ROOT, OUT / "current-routes.json")


def rel(path: Path) -> str:
    return path.relative_to(ROOT).as_posix()


def text(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def line_number(value: str, offset: int) -> int:
    return value.count("\n", 0, offset) + 1


def location(path: str, value: str, offset: int) -> str:
    return f"{path}:{line_number(value, offset)}"


def matching_block(value: str, open_brace: int) -> str:
    """Return a Rust/TS brace block, ignoring braces inside strings/comments.

    This is not a general parser. It is sufficient for the registries inspected
    here and fails closed when a balanced block cannot be found.
    """

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
        elif char in {'"', "'", "`"}:
            quote = char
        elif char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return value[open_brace : index + 1]
        index += 1
    raise ValueError("unbalanced source block")


def meta(artifact: str, source: str, command: str) -> dict[str, str]:
    return {
        "artifact": artifact,
        "status": "CURRENT",
        "lifecycle_status": "GENERATED",
        "source_of_truth": source,
        "last_verified_commit": COMMIT,
        "verification": command,
        "generator": GENERATOR,
    }


def shell_actions() -> list[dict[str, str]]:
    rows: list[dict[str, str]] = []
    pattern = re.compile(
        r"build_secure_action::<\s*([^,>]+)\s*,\s*_\s*>\(\s*[^,]+,\s*\"([^\"]+)\"",
        re.S,
    )
    # Action families may be split into owned submodules as the migration
    # progresses. Keep the public-route inventory independent of file layout.
    for path in sorted((ROOT / "node/src/shell/api/actions").rglob("*.rs")):
        value = path.read_text(encoding="utf-8")
        for found in pattern.finditer(value):
            rows.append(
                {
                    "surface": "signed-shell-action",
                    "path": found.group(2),
                    "request_type": found.group(1).strip(),
                    "transport": "TCP/WebSocket/federation adapters",
                    "source": location(rel(path), value, found.start()),
                }
            )
    return sorted(rows, key=lambda row: row["path"])


def static_http_routes() -> list[dict[str, str]]:
    rows: list[dict[str, str]] = []
    sources = {
        "node/src/drivers/cluster/server.rs": "cluster-admin-and-raft",
        "node/src/shell/storage_http.rs": "public-storage-http",
    }
    pair = re.compile(r'\(\s*"(GET|POST|PUT|DELETE|HEAD|PATCH)"\s*,\s*"([^\"]+)"\s*\)')
    for path, surface in sources.items():
        value = text(path)
        seen: set[tuple[str, str]] = set()
        for found in pair.finditer(value):
            key = (found.group(1), found.group(2))
            if key in seen:
                continue
            seen.add(key)
            rows.append(
                {
                    "surface": surface,
                    "method": key[0],
                    "path": key[1],
                    "source": location(path, value, found.start()),
                }
            )

    storage = text("node/src/shell/storage_http.rs")
    marker = 'p.starts_with("/storage/file/")'
    offset = storage.find(marker)
    if offset >= 0:
        for method in ("GET", "HEAD"):
            rows.append(
                {
                    "surface": "public-storage-http",
                    "method": method,
                    "path": "/storage/file/{id}",
                    "source": location("node/src/shell/storage_http.rs", storage, offset),
                }
            )

    for path, prefix, surface in (
        ("node/src/telemetry/server.rs", "/telemetry/", "telemetry-http"),
        ("node/src/telemetry/pprof.rs", "/debug/pprof", "profiling-http"),
    ):
        value = text(path)
        seen: set[str] = set()
        for found in re.finditer(r'"(/[A-Za-z0-9_./-]+)"', value):
            route = found.group(1)
            if not route.startswith(prefix) or route in seen:
                continue
            seen.add(route)
            rows.append(
                {
                    "surface": surface,
                    "method": "GET",
                    "path": route,
                    "source": location(path, value, found.start()),
                }
            )

    ingress_path = "node/src/drivers/vmm/network/ingress.rs"
    ingress = text(ingress_path)
    rows.extend(
        [
            {
                "surface": "vm-http-ingress",
                "method": "ANY",
                "path": "/{creatureId}/{programId}/{entityId}/{vmId}/{path...}",
                "source": f"{ingress_path}:1",
            },
            {
                "surface": "vm-http-ingress",
                "method": "ANY",
                "path": "/{creatureUsername}/{customPath...}",
                "source": "node/src/drivers/vmm/http_route.rs:1",
            },
        ]
    )
    return sorted(rows, key=lambda row: (row["surface"], row["path"], row["method"]))


def guest_operations() -> list[dict[str, str]]:
    path = "node/src/drivers/vmm/host/vm_host_functions.rs"
    value = text(path)
    anchor = value.index("pub(crate) fn handle_unified_host_call")
    match_at = value.index("match op {", anchor)
    block = matching_block(value, value.index("{", match_at))
    rows: list[dict[str, str]] = []
    arm = re.compile(r'((?:"[^\"]+"\s*\|\s*)*"[^\"]+")\s*=>')
    for found in arm.finditer(block):
        for operation in re.findall(r'"([^\"]+)"', found.group(1)):
            rows.append(
                {
                    "operation": operation,
                    "gateway": "unified-host-call",
                    "source": location(path, value, match_at + 1 + found.start()),
                }
            )
    unique = {row["operation"]: row for row in rows}
    return [unique[key] for key in sorted(unique)]


def route_inventory() -> dict[str, Any]:
    actions = shell_actions()
    http = static_http_routes()
    guest = guest_operations()
    return {
        "_meta": meta(
            "A002",
            "legacy action registration and HTTP/guest dispatch source",
            "python3 scripts/generate_current_surface_inventories.py --check",
        ),
        "summary": {
            "signed_shell_actions": len(actions),
            "http_routes": len(http),
            "guest_operations": len(guest),
            "node_facing_vmm_api": "absent; current VMM ingress is guest/public workload ingress",
            "federation_api": "custom framed action transport; signed shell paths are carried in envelopes",
        },
        "signed_shell_actions": actions,
        "http_routes": http,
        "guest_operations": guest,
    }


def sample_env() -> dict[str, str]:
    values: dict[str, str] = {}
    for line in text("node/sample.env").splitlines():
        found = re.match(r"^([A-Z][A-Z0-9_]*)=(.*)$", line.strip())
        if found:
            values[found.group(1)] = found.group(2).strip().strip('"')
    return values


def configuration_inventory() -> dict[str, Any]:
    occurrences: dict[str, list[dict[str, str]]] = defaultdict(list)
    source_paths = sorted(
        list((ROOT / "node/src").rglob("*.rs"))
        + list((ROOT / "vms").rglob("*.rs"))
        + list((ROOT / "cmd/casparctl/src").rglob("*.rs"))
        + list((ROOT / "crates/aseman-config/src").rglob("*.rs"))
        + [ROOT / "client-cli/index.ts"]
    )
    rust_patterns = [
        ("read", re.compile(r'(?:std::)?env::var(?:_os)?\(\s*"([A-Z][A-Z0-9_]*)"')),
        ("helper-read", re.compile(r'(?:env_f64|env_i64|env_trimmed|env_override)\(\s*"([A-Z][A-Z0-9_]*)"')),
        ("typescript-read", re.compile(r'process\.env\.([A-Z][A-Z0-9_]*)')),
    ]
    for path in source_paths:
        value = path.read_text(encoding="utf-8")
        for kind, pattern in rust_patterns:
            for found in pattern.finditer(value):
                occurrences[found.group(1)].append(
                    {"kind": kind, "source": location(rel(path), value, found.start())}
                )

    # Once a direct read moves behind `aseman-config`, the legacy spelling must
    # remain inventoried for the whole ADR-0004 compatibility window. The A103
    # alias contract is therefore also a discoverable configuration surface;
    # otherwise regenerating this inventory would silently erase aliases as
    # soon as their scattered consumers are correctly deleted.
    alias_path = ROOT / "contracts/config/legacy-aliases.json"
    if alias_path.exists():
        alias_text = alias_path.read_text(encoding="utf-8")
        alias_contract = json.loads(alias_text)
        for row in alias_contract.get("aliases", []):
            key = row["legacy"]
            marker = f'"legacy": "{key}"'
            offset = alias_text.find(marker)
            occurrences[key].append(
                {
                    "kind": "compatibility-alias",
                    "source": location(rel(alias_path), alias_text, max(offset, 0)),
                }
            )

    samples = sample_env()
    for key in samples:
        sample_text = text("node/sample.env")
        offset = sample_text.find(f"{key}=")
        occurrences[key].append(
            {"kind": "sample-declaration", "source": location("node/sample.env", sample_text, offset)}
        )

    runner = text("run-nodes.sh")
    heredoc = re.search(r'cat > "\$env_file" <<EOF\n(?P<body>.*?)\nEOF', runner, re.S)
    if heredoc:
        for found in re.finditer(r'^([A-Z][A-Z0-9_]*)=', heredoc.group("body"), re.M):
            key = found.group(1)
            absolute = heredoc.start("body") + found.start()
            occurrences[key].append(
                {"kind": "deployment-write", "source": location("run-nodes.sh", runner, absolute)}
            )

    dockerfile = text("node/Dockerfile")
    for found in re.finditer(r'^(ENV|ARG)\s+([A-Z][A-Z0-9_]*)', dockerfile, re.M):
        occurrences[found.group(2)].append(
            {"kind": f"docker-{found.group(1).lower()}", "source": location("node/Dockerfile", dockerfile, found.start())}
        )

    def category(key: str) -> str:
        if re.search(r"PRIVATE|SECRET|TOKEN|PASSWORD|API_KEY|AUTH_TOKEN|KEY_PATH", key):
            return "secret-or-sensitive"
        if re.search(r"PORT|ENDPOINT|URL|HOST|ADDR|ORIGIN|IPADDR", key):
            return "network"
        if key in {"HOME", "PATH", "USERPROFILE", "LD_LIBRARY_PATH"}:
            return "process-environment"
        if key.startswith(("VM_", "CASPAR_", "CLUSTER_", "RATE_LIMIT_", "MODAL_")):
            return "application"
        return "runtime-or-storage"

    keys = []
    for key in sorted(occurrences):
        deduped = {(item["kind"], item["source"]): item for item in occurrences[key]}
        keys.append(
            {
                "key": key,
                "category": category(key),
                "sample_value": samples.get(key),
                "occurrences": sorted(deduped.values(), key=lambda item: (item["source"], item["kind"])),
            }
        )
    return {
        "_meta": meta(
            "A003",
            "literal environment reads, sample.env, deployment env generation, and Docker build args",
            "python3 scripts/generate_current_surface_inventories.py --check",
        ),
        "summary": {
            "keys": len(keys),
            "secret_or_sensitive": sum(row["category"] == "secret-or-sensitive" for row in keys),
            "network": sum(row["category"] == "network" for row in keys),
            "limitation": "computed/dynamic keys and effective defaults still require semantic review",
        },
        "keys": keys,
    }


def runtime_inventory() -> dict[str, Any]:
    aggregator_path = "node/crates/caspar-vm-plugins/src/lib.rs"
    aggregator = text(aggregator_path)
    enabled_match = re.search(r"pub fn enabled_vm_keys\(\).*?vec!\[(.*?)\]", aggregator, re.S)
    enabled = set(re.findall(r'"([^\"]+)"', enabled_match.group(1))) if enabled_match else set()

    trait_path = "vm-sdk/src/plugin.rs"
    trait_source = text(trait_path)
    trait_anchor = trait_source.index("pub trait VmPlugin")
    trait_block = matching_block(trait_source, trait_source.index("{", trait_anchor))
    operations = sorted(set(re.findall(r"\bfn\s+([a-zA-Z0-9_]+)\s*\(", trait_block)))

    runtimes: list[dict[str, Any]] = []
    for config_path in sorted((ROOT / "vms").glob("*/vm.config.json")):
        config = json.loads(config_path.read_text(encoding="utf-8"))
        controller_path = config_path.parent / "src/controller.rs"
        controller = controller_path.read_text(encoding="utf-8")
        impl_found = re.search(r"impl\s+VmPlugin\s+for\s+[A-Za-z0-9_]+\s*\{", controller)
        if not impl_found:
            raise ValueError(f"VmPlugin implementation not found in {rel(controller_path)}")
        block = matching_block(controller, controller.find("{", impl_found.start()))
        overrides = sorted(set(re.findall(r"\bfn\s+([a-zA-Z0-9_]+)\s*\(", block)))
        operation_modes = {
            operation: "override" if operation in overrides else "inherited-default"
            for operation in operations
        }
        runtimes.append(
            {
                "key": config["key"],
                "name": config["name"],
                "version": config["version"],
                "enabled_in_generated_aggregator": config["key"] in enabled,
                "config": rel(config_path),
                "implementation": rel(controller_path),
                "aliases": config.get("aliases", []),
                "artifact_extensions": config.get("artifactExtensions", []),
                "in_process": config.get("inProcess", False),
                "default_runtime": config.get("defaultRuntime", False),
                "restorable": config.get("restorable", False),
                "supports_chain_transactions": config.get("supportsChainTrxs", False),
                "provides_program_verification": config.get("providesProgramVerification", False),
                "operations": operation_modes,
                "overrides": overrides,
            }
        )
    return {
        "_meta": meta(
            "A006",
            "vms/*/vm.config.json, VmPlugin trait, controllers, and generated aggregator",
            "python3 scripts/generate_current_surface_inventories.py --check",
        ),
        "summary": {
            "runtime_count": len(runtimes),
            "enabled_count": sum(item["enabled_in_generated_aggregator"] for item in runtimes),
            "trait_operation_count": len(operations),
            "selection_model": "compile-time generated aggregation",
        },
        "trait_operations": operations,
        "runtimes": runtimes,
    }


def rust_match_commands(path: str, anchor: str) -> list[dict[str, str]]:
    value = text(path)
    start = value.index(anchor)
    match_start = value.index("match", start)
    block = matching_block(value, value.index("{", match_start))
    rows: list[dict[str, str]] = []
    for found in re.finditer(r'((?:"[^\"]+"\s*\|\s*)*"[^\"]+")\s*=>\s*([^,\n{]+)', block):
        handler = found.group(2).strip()
        for command in re.findall(r'"([^\"]+)"', found.group(1)):
            if command in {"help", "-h", "--help"}:
                continue
            rows.append(
                {
                    "command": command,
                    "handler": handler,
                    "source": location(path, value, match_start + 1 + found.start()),
                }
            )
    return rows


def cli_inventory() -> dict[str, Any]:
    casparctl = {
        "top_level": rust_match_commands("cmd/casparctl/src/main.rs", "fn main()"),
        "vms": rust_match_commands("cmd/casparctl/src/vms.rs", "pub fn run_vms"),
        "cluster": rust_match_commands("cmd/casparctl/src/cluster.rs", "pub fn run_cluster"),
        "pprof": [
            {"command": command, "source": f"cmd/casparctl/src/main.rs:{line}"}
            for command, line in (("runtime", 1221), ("heap", 1222), ("threads", 1223), ("flamegraph", 1224), ("profile", 1225))
        ],
        "cluster_config": [
            {"command": command, "source": "cmd/casparctl/src/cluster.rs:261"}
            for command in ("list", "get", "set")
        ],
    }

    client_path = "client-cli/index.ts"
    client = text(client_path)
    table_at = client.index("const commands:")
    assignment = re.search(r"}\s*=\s*{", client[table_at:])
    if assignment is None:
        raise ValueError("client command table assignment not found")
    open_at = table_at + assignment.end() - 1
    table = matching_block(client, open_at)
    client_commands = []
    command_pattern = re.compile(r'^\s{2}(?:"([^\"]+)"|([A-Za-z][A-Za-z0-9_]*)):\s*async\s*\(', re.M)
    for found in command_pattern.finditer(table):
        client_commands.append(
            {
                "command": found.group(1) or found.group(2),
                "source": location(client_path, client, open_at + found.start()),
            }
        )

    scripts = []
    for path in sorted(ROOT.glob("*.sh")):
        value = path.read_text(encoding="utf-8")
        flags = []
        for found in re.finditer(r'^\s*(--[a-z][a-z0-9-]*)(?:\|-[a-z])?\)', value, re.M):
            flags.append(
                {"flag": found.group(1), "source": location(rel(path), value, found.start())}
            )
        scripts.append({"script": rel(path), "flags": flags})

    return {
        "_meta": meta(
            "A007",
            "casparctl/client-cli dispatch tables and root script option cases",
            "python3 scripts/generate_current_surface_inventories.py --check",
        ),
        "summary": {
            "casparctl_top_level_commands": len(casparctl["top_level"]),
            "client_cli_commands": len(client_commands),
            "root_scripts": len(scripts),
        },
        "casparctl": casparctl,
        "client_cli": client_commands,
        "root_scripts": scripts,
    }


def front_matter(source: str) -> list[str]:
    return [
        "---",
        "status: CURRENT",
        "owner: migration/P0-01",
        f"source_of_truth: {source}",
        f"last_verified_commit: {COMMIT}",
        f"verification: python3 {GENERATOR} --check",
        "---",
    ]


def routes_markdown(data: dict[str, Any]) -> str:
    lines = front_matter("legacy route/action dispatch source") + [
        "",
        "# Current route and operation inventory",
        "",
        f"> Generated by `{GENERATOR}`. Do not edit by hand.",
        "",
        "This records current legacy surfaces. It is not the target HTTP/VMM contract.",
        "",
        f"- Signed shell actions: {data['summary']['signed_shell_actions']}",
        f"- HTTP routes/patterns: {data['summary']['http_routes']}",
        f"- Guest host operations and aliases: {data['summary']['guest_operations']}",
        "- A node-facing VMM control API does not currently exist.",
        "- Federation carries signed action paths over a custom framed transport.",
        "",
        "## Signed shell actions",
        "",
        "| Path | Request type | Source |",
        "|---|---|---|",
    ]
    for row in data["signed_shell_actions"]:
        lines.append(f"| `{row['path']}` | `{row['request_type']}` | `{row['source']}` |")
    lines += ["", "## HTTP routes", "", "| Surface | Method | Path | Source |", "|---|---|---|---|"]
    for row in data["http_routes"]:
        lines.append(f"| {row['surface']} | {row['method']} | `{row['path']}` | `{row['source']}` |")
    lines += [
        "",
        "## Guest gateway",
        "",
        "The complete operation/alias list and source locations are in `current-routes.json`.",
        "Guest identity and authorization semantics still require A005/A010 review before extraction.",
        "",
    ]
    return "\n".join(lines)


def configuration_markdown(data: dict[str, Any]) -> str:
    lines = front_matter("literal environment/configuration reads and deployment writers") + [
        "",
        "# Current configuration inventory",
        "",
        f"> Generated by `{GENERATOR}`. Do not edit by hand.",
        "",
        f"Discovered {data['summary']['keys']} keys. Values are never read from the environment by this generator.",
        "Effective defaults hidden in implementation code remain a semantic-review item.",
        "",
        "| Key | Category | sample.env value | Occurrences |",
        "|---|---|---|---:|",
    ]
    for row in data["keys"]:
        sample = row["sample_value"]
        shown = "—" if sample is None else ("(empty)" if sample == "" else f"`{sample}`")
        lines.append(f"| `{row['key']}` | {row['category']} | {shown} | {len(row['occurrences'])} |")
    lines.append("")
    return "\n".join(lines)


def runtime_markdown(data: dict[str, Any]) -> str:
    lines = front_matter("VM configs, VmPlugin trait/implementations, generated aggregator") + [
        "",
        "# Current runtime capability matrix",
        "",
        f"> Generated by `{GENERATOR}`. Do not edit by hand.",
        "",
        "`override` means the runtime implements the trait method directly; `inherited-default`",
        "means behavior comes from `vm-sdk/src/plugin.rs` and must not be mistaken for native support.",
        "",
        "| Runtime | Enabled | In process | Default | Restorable | Chain transactions | Verification | Overrides |",
        "|---|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for row in data["runtimes"]:
        yes = lambda value: "yes" if value else "no"
        lines.append(
            f"| `{row['key']}` | {yes(row['enabled_in_generated_aggregator'])} | {yes(row['in_process'])} | "
            f"{yes(row['default_runtime'])} | {yes(row['restorable'])} | {yes(row['supports_chain_transactions'])} | "
            f"{yes(row['provides_program_verification'])} | {len(row['overrides'])} |"
        )
    lines += [
        "",
        "The complete per-operation override matrix is in `current-runtime-matrix.json`.",
        "All enabled runtimes are statically compiled through `caspar-vm-plugins`; runtime replacement is not dynamic.",
        "",
    ]
    return "\n".join(lines)


def cli_markdown(data: dict[str, Any]) -> str:
    lines = front_matter("CLI dispatch tables and root shell-script option cases") + [
        "",
        "# Current CLI and operational-script inventory",
        "",
        f"> Generated by `{GENERATOR}`. Do not edit by hand.",
        "",
        "## casparctl",
        "",
    ]
    for group in ("top_level", "vms", "cluster", "cluster_config", "pprof"):
        commands = ", ".join(f"`{row['command']}`" for row in data["casparctl"][group])
        lines.append(f"- {group.replace('_', ' ')}: {commands or '—'}")
    lines += [
        "",
        "## caspar-client",
        "",
        f"The TypeScript client exposes {len(data['client_cli'])} dispatch-table commands; the complete list is in `current-cli-ops.json`.",
        "",
        "## Root operational scripts",
        "",
        "| Script | Parsed interface flags |",
        "|---|---|",
    ]
    for script in data["root_scripts"]:
        flags = ", ".join(f"`{row['flag']}`" for row in script["flags"]) or "—"
        lines.append(f"| `{script['script']}` | {flags} |")
    lines += [
        "",
        "These scripts are current imperative operations and are not the target bootstrap/operations interface.",
        "",
    ]
    return "\n".join(lines)


def outputs() -> dict[Path, str]:
    routes = route_inventory()
    config = configuration_inventory()
    runtimes = runtime_inventory()
    cli = cli_inventory()
    records = {
        OUT / "current-routes.json": json.dumps(routes, indent=2, sort_keys=True) + "\n",
        OUT / "current-routes.md": routes_markdown(routes),
        OUT / "current-configuration.json": json.dumps(config, indent=2, sort_keys=True) + "\n",
        OUT / "current-configuration.md": configuration_markdown(config),
        OUT / "current-runtime-matrix.json": json.dumps(runtimes, indent=2, sort_keys=True) + "\n",
        OUT / "current-runtime-matrix.md": runtime_markdown(runtimes),
        OUT / "current-cli-ops.json": json.dumps(cli, indent=2, sort_keys=True) + "\n",
        OUT / "current-cli-ops.md": cli_markdown(cli),
    }
    return records


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
        print("current surface inventories are up to date")
        return 0
    for path, value in records.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(value, encoding="utf-8")
        print(f"wrote {rel(path)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
