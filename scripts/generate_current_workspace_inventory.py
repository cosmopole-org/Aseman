#!/usr/bin/env python3
"""Generate the Phase 0 current workspace inventory.

This script intentionally parses manifests instead of invoking a build.  The
legacy repository has several independent Cargo roots and expensive native/git
dependencies, so inventory generation must remain fast and offline.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tomllib
from collections import Counter
from pathlib import Path
from typing import Any

from inventory_common import retained_revision


ROOT = Path(__file__).resolve().parents[1]
JSON_PATH = ROOT / "docs/generated/current-workspace.json"
MARKDOWN_PATH = ROOT / "docs/generated/current-workspace.md"
IGNORED_PARTS = {".git", "dist", "target", "node_modules"}


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args],
        cwd=ROOT,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    ).stdout.strip()


def relative(path: Path) -> str:
    return path.relative_to(ROOT).as_posix()


def discover(filename: str) -> list[Path]:
    return sorted(
        path
        for path in ROOT.rglob(filename)
        if not any(part in IGNORED_PARTS for part in path.relative_to(ROOT).parts)
    )


def dependency_rows(manifest: dict[str, Any], manifest_dir: Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []

    def add_table(table: dict[str, Any], scope: str, target: str | None = None) -> None:
        for name, raw in sorted(table.items()):
            spec = raw if isinstance(raw, dict) else {"version": raw}
            row: dict[str, Any] = {"name": name, "scope": scope}
            if target is not None:
                row["target"] = target
            for key in (
                "version",
                "package",
                "git",
                "rev",
                "tag",
                "branch",
                "optional",
                "default-features",
            ):
                if key in spec:
                    row[key] = spec[key]
            if "features" in spec:
                row["features"] = sorted(spec["features"])
            if "path" in spec:
                dependency_path = (manifest_dir / spec["path"]).resolve()
                try:
                    row["path"] = relative(dependency_path)
                except ValueError:
                    row["path"] = str(dependency_path)
            rows.append(row)

    table_names = {
        "dependencies": "normal",
        "dev-dependencies": "dev",
        "build-dependencies": "build",
    }
    for table_name, scope in table_names.items():
        add_table(manifest.get(table_name, {}), scope)

    for target, target_config in sorted(manifest.get("target", {}).items()):
        for table_name, scope in table_names.items():
            add_table(target_config.get(table_name, {}), scope, target)

    return rows


def targets(manifest: dict[str, Any], manifest_dir: Path) -> list[dict[str, str]]:
    package = manifest.get("package", {})
    result: list[dict[str, str]] = []

    lib = manifest.get("lib")
    if isinstance(lib, dict):
        result.append(
            {
                "kind": "lib",
                "name": lib.get("name", package.get("name", "")),
                "path": lib.get("path", "src/lib.rs"),
            }
        )
    elif (manifest_dir / "src/lib.rs").exists():
        result.append(
            {
                "kind": "lib",
                "name": package.get("name", ""),
                "path": "src/lib.rs",
            }
        )

    binaries = manifest.get("bin", [])
    if isinstance(binaries, dict):
        binaries = [binaries]
    for binary in binaries:
        result.append(
            {
                "kind": "bin",
                "name": binary.get("name", package.get("name", "")),
                "path": binary.get("path", "src/main.rs"),
            }
        )
    if not binaries and (manifest_dir / "src/main.rs").exists():
        result.append(
            {
                "kind": "bin",
                "name": package.get("name", ""),
                "path": "src/main.rs",
            }
        )
    return sorted(result, key=lambda item: (item["kind"], item["name"]))


def cargo_inventory() -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    packages: list[dict[str, Any]] = []
    workspaces: list[dict[str, Any]] = []
    for path in discover("Cargo.toml"):
        manifest = tomllib.loads(path.read_text(encoding="utf-8"))
        manifest_dir = path.parent
        workspace = manifest.get("workspace")
        if isinstance(workspace, dict):
            workspaces.append(
                {
                    "manifest": relative(path),
                    "resolver": workspace.get("resolver"),
                    "members": sorted(workspace.get("members", [])),
                    "exclude": sorted(workspace.get("exclude", [])),
                }
            )

        package = manifest.get("package")
        if not isinstance(package, dict):
            continue
        packages.append(
            {
                "manifest": relative(path),
                "name": package.get("name"),
                "version": package.get("version"),
                "edition": package.get("edition", "2015"),
                "description": package.get("description", ""),
                "features": {
                    name: sorted(value)
                    for name, value in sorted(manifest.get("features", {}).items())
                },
                "targets": targets(manifest, manifest_dir),
                "dependencies": dependency_rows(manifest, manifest_dir),
            }
        )
    return packages, workspaces


def npm_inventory() -> list[dict[str, Any]]:
    packages: list[dict[str, Any]] = []
    for path in discover("package.json"):
        package = json.loads(path.read_text(encoding="utf-8"))
        packages.append(
            {
                "manifest": relative(path),
                "name": package.get("name"),
                "version": package.get("version"),
                "type": package.get("type"),
                "scripts": package.get("scripts", {}),
                "dependencies": package.get("dependencies", {}),
                "devDependencies": package.get("devDependencies", {}),
            }
        )
    return packages


def repository_inventory() -> dict[str, Any]:
    tracked = [line for line in git("ls-files").splitlines() if line]
    top_level = Counter(path.split("/", 1)[0] for path in tracked)
    source_suffixes = {".rs", ".ts", ".py", ".sh"}
    source_counts = Counter(
        Path(path).suffix for path in tracked if Path(path).suffix in source_suffixes
    )
    return {
        "tracked_file_count": len(tracked),
        "tracked_files_by_top_level": dict(sorted(top_level.items())),
        "tracked_source_files_by_extension": dict(sorted(source_counts.items())),
        "tracked_dist_file_count": sum(path.startswith("dist/") for path in tracked),
        "lockfiles": [relative(path) for path in discover("Cargo.lock")]
        + [relative(path) for path in discover("package-lock.json")],
    }


def build_inventory() -> dict[str, Any]:
    rust_packages, rust_workspaces = cargo_inventory()
    return {
        "_meta": {
            "artifact": "A001",
            "status": "CURRENT",
            "source_of_truth": "Cargo.toml/package.json manifests and the Git index",
            "last_verified_commit": retained_revision(ROOT, JSON_PATH),
            "verification": "python3 scripts/generate_current_workspace_inventory.py --check",
            "generator": "scripts/generate_current_workspace_inventory.py",
        },
        "repository": repository_inventory(),
        "rust": {
            "workspaces": rust_workspaces,
            "packages": rust_packages,
        },
        "npm": {"packages": npm_inventory()},
    }


def markdown(inventory: dict[str, Any]) -> str:
    meta = inventory["_meta"]
    repository = inventory["repository"]
    rust = inventory["rust"]
    npm = inventory["npm"]
    lines = [
        "---",
        "status: CURRENT",
        "owner: migration/P0-01",
        "source_of_truth: Cargo.toml/package.json manifests and the Git index",
        f"last_verified_commit: {meta['last_verified_commit']}",
        f"verification: {meta['verification']}",
        "---",
        "",
        "# Current workspace inventory",
        "",
        "> Generated by `scripts/generate_current_workspace_inventory.py`. Do not edit by hand.",
        "",
        "This is the Phase 0 / A001 inventory of the legacy repository before workspace migration.",
        "The complete dependency and feature data is in `current-workspace.json`.",
        "",
        "## Repository summary",
        "",
        f"- Tracked files: {repository['tracked_file_count']}",
        f"- Tracked files under `dist/`: {repository['tracked_dist_file_count']}",
        f"- Rust package manifests: {len(rust['packages'])}",
        f"- Declared Cargo workspace roots: {len(rust['workspaces'])}",
        f"- npm package manifests: {len(npm['packages'])}",
        f"- Lockfiles: {', '.join(f'`{path}`' for path in repository['lockfiles'])}",
        "",
        "There is no root Cargo workspace. The node and Wasm plugin declare independent",
        "workspaces; the remaining Rust packages are reached as path dependencies or built",
        "from separate manifests. This is current behavior, not the target topology.",
        "",
        "## Cargo workspaces",
        "",
        "| Manifest | Resolver | Members | Excludes |",
        "|---|---:|---|---|",
    ]
    for workspace in rust["workspaces"]:
        members = ", ".join(f"`{member}`" for member in workspace["members"]) or "—"
        excludes = ", ".join(f"`{item}`" for item in workspace["exclude"]) or "—"
        lines.append(
            f"| `{workspace['manifest']}` | {workspace['resolver'] or 'default'} | {members} | {excludes} |"
        )

    lines.extend(
        [
            "",
            "## Rust packages",
            "",
            "| Package | Manifest | Edition | Targets | Direct dependencies | Features |",
            "|---|---|---:|---|---:|---:|",
        ]
    )
    for package in rust["packages"]:
        target_names = ", ".join(
            f"{target['kind']}:{target['name']}" for target in package["targets"]
        ) or "—"
        lines.append(
            f"| `{package['name']}` | `{package['manifest']}` | {package['edition']} | "
            f"{target_names} | {len(package['dependencies'])} | {len(package['features'])} |"
        )

    path_edges: list[tuple[str, dict[str, Any]]] = []
    for package in rust["packages"]:
        path_edges.extend(
            (package["name"], dependency)
            for dependency in package["dependencies"]
            if "path" in dependency
        )
    lines.extend(
        [
            "",
            "## Local Rust dependency edges",
            "",
            "| From | Scope | Dependency | Path |",
            "|---|---|---|---|",
        ]
    )
    for package_name, dependency in sorted(
        path_edges, key=lambda item: (item[0], item[1]["name"], item[1]["scope"])
    ):
        lines.append(
            f"| `{package_name}` | {dependency['scope']} | `{dependency['name']}` | "
            f"`{dependency['path']}` |"
        )

    lines.extend(
        [
            "",
            "## npm packages",
            "",
            "| Package | Manifest | Runtime dependencies | Development dependencies | Scripts |",
            "|---|---|---:|---:|---:|",
        ]
    )
    for package in npm["packages"]:
        lines.append(
            f"| `{package['name']}` | `{package['manifest']}` | "
            f"{len(package['dependencies'])} | {len(package['devDependencies'])} | "
            f"{len(package['scripts'])} |"
        )

    lines.extend(
        [
            "",
            "## Phase 1 implications",
            "",
            "- The future root workspace must account for both existing workspace roots and all path dependencies.",
            "- The two committed Cargo lockfiles must not be collapsed until the root-workspace build is reproducible.",
            "- Runtime crates are currently compile-time dependencies of `caspar-node` through the generated aggregator.",
            "- `dist/` is tracked release/runtime material and requires a separate removal-ledger entry before deletion.",
            "",
        ]
    )
    return "\n".join(lines)


def serialized(inventory: dict[str, Any]) -> dict[Path, str]:
    return {
        JSON_PATH: json.dumps(inventory, indent=2, sort_keys=True) + "\n",
        MARKDOWN_PATH: markdown(inventory),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail when committed generated files differ from current manifests",
    )
    args = parser.parse_args()
    outputs = serialized(build_inventory())
    if args.check:
        stale = [path for path, content in outputs.items() if not path.exists() or path.read_text() != content]
        if stale:
            for path in stale:
                print(f"stale: {relative(path)}", file=sys.stderr)
            return 1
        print("current workspace inventory is up to date")
        return 0

    for path, content in outputs.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
        print(f"wrote {relative(path)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
