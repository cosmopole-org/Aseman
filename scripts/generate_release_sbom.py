#!/usr/bin/env python3
"""Generate a deterministic SPDX 2.3 release SBOM from Cargo.lock and artifacts."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def spdx_id(prefix: str, value: str, ordinal: int) -> str:
    safe = re.sub(r"[^A-Za-z0-9.-]+", "-", value).strip("-") or "unknown"
    return f"SPDXRef-{prefix}-{safe}-{ordinal}"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def cargo_metadata() -> dict:
    completed = subprocess.run(
        ["cargo", "metadata", "--locked", "--format-version", "1"],
        cwd=ROOT,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    return json.loads(completed.stdout)


def created_at() -> str:
    raw = os.environ.get("SOURCE_DATE_EPOCH")
    if raw is None or not raw.isdigit():
        raise ValueError("SOURCE_DATE_EPOCH must be a non-negative integer")
    return dt.datetime.fromtimestamp(int(raw), tz=dt.UTC).strftime("%Y-%m-%dT%H:%M:%SZ")


def build(artifact_root: Path, name: str, version: str, namespace: str) -> dict:
    metadata = cargo_metadata()
    packages = sorted(metadata["packages"], key=lambda row: (row["name"], row["version"], row["id"]))
    package_ids: dict[str, str] = {}
    spdx_packages = []
    for index, package in enumerate(packages, start=1):
        identifier = spdx_id("Package", f"{package['name']}-{package['version']}", index)
        package_ids[package["id"]] = identifier
        source = package.get("source") or "NOASSERTION"
        spdx_packages.append(
            {
                "SPDXID": identifier,
                "name": package["name"],
                "versionInfo": package["version"],
                "downloadLocation": source,
                "filesAnalyzed": False,
                "licenseConcluded": "NOASSERTION",
                "licenseDeclared": package.get("license") or "NOASSERTION",
                "copyrightText": "NOASSERTION",
                "externalRefs": [
                    {
                        "referenceCategory": "PACKAGE-MANAGER",
                        "referenceType": "purl",
                        "referenceLocator": f"pkg:cargo/{package['name']}@{package['version']}",
                    }
                ],
            }
        )

    distribution_id = "SPDXRef-Package-Aseman-Distribution"
    spdx_packages.insert(
        0,
        {
            "SPDXID": distribution_id,
            "name": name,
            "versionInfo": version,
            "downloadLocation": "NOASSERTION",
            "filesAnalyzed": True,
            "licenseConcluded": "NOASSERTION",
            "licenseDeclared": "NOASSERTION",
            "copyrightText": "NOASSERTION",
        },
    )

    files = []
    relationships = [{"spdxElementId": "SPDXRef-DOCUMENT", "relationshipType": "DESCRIBES", "relatedSpdxElement": distribution_id}]
    for index, path in enumerate(sorted(p for p in artifact_root.rglob("*") if p.is_file()), start=1):
        relative = path.relative_to(artifact_root).as_posix()
        identifier = spdx_id("File", relative, index)
        files.append(
            {
                "SPDXID": identifier,
                "fileName": f"./{relative}",
                "checksums": [{"algorithm": "SHA256", "checksumValue": sha256(path)}],
                "licenseConcluded": "NOASSERTION",
                "copyrightText": "NOASSERTION",
            }
        )
        relationships.append({"spdxElementId": distribution_id, "relationshipType": "CONTAINS", "relatedSpdxElement": identifier})

    resolve = metadata.get("resolve") or {}
    for node in sorted(resolve.get("nodes", []), key=lambda row: row["id"]):
        source_id = package_ids.get(node["id"])
        if source_id is None:
            continue
        for dependency in sorted(node.get("dependencies", [])):
            target_id = package_ids.get(dependency)
            if target_id is not None:
                relationships.append({"spdxElementId": source_id, "relationshipType": "DEPENDS_ON", "relatedSpdxElement": target_id})

    workspace = set(metadata.get("workspace_members", []))
    for package_id in sorted(workspace):
        if package_id in package_ids:
            relationships.append({"spdxElementId": distribution_id, "relationshipType": "GENERATED_FROM", "relatedSpdxElement": package_ids[package_id]})

    return {
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": name,
        "documentNamespace": namespace,
        "creationInfo": {"created": created_at(), "creators": ["Tool: scripts/generate_release_sbom.py"]},
        "packages": spdx_packages,
        "files": files,
        "relationships": relationships,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifact-root", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--name", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--namespace", required=True)
    args = parser.parse_args()
    if not args.artifact_root.is_dir():
        parser.error(f"artifact root does not exist: {args.artifact_root}")
    document = build(args.artifact_root, args.name, args.version, args.namespace)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"wrote {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
