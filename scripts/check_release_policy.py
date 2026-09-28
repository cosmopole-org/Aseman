#!/usr/bin/env python3
"""Validate A906 release policy, workflow, licenses, and optional release bundle."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
POLICY_PATH = ROOT / "contracts/release/policy-v1.json"
WORKFLOW_PATH = ROOT / ".github/workflows/release.yml"
RUNTIME_PATH = ROOT / "contracts/release/runtime-dependencies.json"
INSTALLER_PATH = ROOT / "scripts/install.sh"
SHA = re.compile(r"^[0-9a-f]{40}$")


def load_json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def cargo_packages() -> list[dict]:
    completed = subprocess.run(
        ["cargo", "metadata", "--locked", "--format-version", "1"],
        cwd=ROOT,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    return json.loads(completed.stdout)["packages"]


def license_tokens(expression: str) -> set[str]:
    return {
        token
        for token in re.findall(r"[A-Za-z0-9][A-Za-z0-9.+-]*(?: WITH [A-Za-z0-9.-]+)?", expression)
        if token not in {"AND", "OR", "WITH"}
    }


def check_repository(policy: dict) -> list[str]:
    problems: list[str] = []
    if policy.get("schema_version") != 1 or policy.get("artifact") != "A906":
        problems.append("unsupported release policy identity")
    release = policy.get("release", {})
    try:
        re.compile(release.get("tag_pattern", ""))
    except re.error:
        problems.append("release tag pattern is invalid")
    for field in ("locked_dependencies", "source_commit_bound", "source_date_epoch_required"):
        if release.get(field) is not True:
            problems.append(f"release.{field} must be true")
    if release.get("tracked_output_forbidden") != "dist/":
        problems.append("tracked dist/ must remain forbidden as a release destination")

    artifacts = policy.get("artifacts", {})
    expected = {
        "aseman-node", "aseman-keygen", "asemanctl", "aseman-vmm", "aseman-meter",
        "aseman-vmm-agent", "aseman-vmm-backend-nomad", "aseman-vmm-backend-native",
    }
    if set(artifacts.get("required_binaries", [])) != expected:
        problems.append("canonical release binary set drifted")
    if artifacts.get("checksum_algorithm") != "SHA256":
        problems.append("release checksums must use SHA256")
    sbom = policy.get("sbom", {})
    if sbom.get("format") != "SPDX-2.3" or sbom.get("file_hash_algorithm") != "SHA256":
        problems.append("release SBOM must be SPDX 2.3 with SHA256 file hashes")
    provenance = policy.get("provenance", {})
    if provenance.get("predicate_type") != "https://slsa.dev/provenance/v1":
        problems.append("provenance predicate must be SLSA v1")
    if provenance.get("subject_digest") != "sha256":
        problems.append("provenance must bind SHA256 subjects")

    workflow = WORKFLOW_PATH.read_text(encoding="utf-8")
    for forbidden in ("git add", "git push", "git commit", "dist/"):
        if forbidden in workflow:
            problems.append(f"release workflow writes the source tree or tracked dist/: {forbidden}")
    # Only the publish job may write, and it writes a GitHub Release.
    publish = workflow.split("\n  publish:", 1)
    if workflow.count("contents: write") != 1 or len(publish) != 2 or "contents: write" not in publish[1]:
        problems.append("contents: write must appear once, in the publish job only")
    tracked = subprocess.run(
        ["git", "ls-files", "dist"], cwd=ROOT, check=True, stdout=subprocess.PIPE, text=True
    ).stdout.strip()
    if tracked:
        problems.append("dist/ is tracked; release binaries are published, never committed")
    installer = INSTALLER_PATH.read_text(encoding="utf-8")
    stage = (ROOT / "scripts/stage-release.sh").read_text(encoding="utf-8")
    runtime = load_json(RUNTIME_PATH)
    for name, dependency in runtime.get("dependencies", {}).items():
        for arch, row in dependency.get("archives", {}).items():
            if not re.fullmatch(r"[0-9a-f]{64}", row.get("sha256", "")):
                problems.append(f"{name}/{arch} has no SHA-256 pin")
            for label, text in (("scripts/install.sh", installer), ("the release workflow", workflow)):
                if name == "firecracker" and label == "the release workflow":
                    continue
                if row["url"] not in text or row["sha256"] not in text:
                    problems.append(f"{label} does not carry the {name}/{arch} pin")
    for binary in artifacts.get("required_binaries", []):
        if binary not in stage or binary not in installer:
            problems.append(f"{binary} is not staged by stage-release.sh and installed by install.sh")
    for permission in policy.get("ci", {}).get("required_permissions", []):
        if permission not in workflow:
            problems.append(f"release workflow omits permission {permission}")
    if "actions/attest@" not in workflow or "sbom-path:" not in workflow:
        problems.append("release workflow must attest provenance and the SBOM")
    if "rustsec/audit-check@" not in workflow:
        problems.append("release workflow must audit Cargo.lock against RustSec")
    if "anchore/scan-action@" not in workflow:
        problems.append("release workflow must scan packaged artifacts")
    for required in ("fail-build: true", "severity-cutoff: high"):
        if required not in workflow:
            problems.append(f"artifact vulnerability scan omits {required}")
    if "scripts/stage-release.sh" not in workflow or "cargo build --release --locked" not in stage:
        problems.append("the workflow must build through the locked scripts/stage-release.sh")
    uses = re.findall(r"^\s*uses:\s*[^@\s]+@([^\s#]+)", workflow, re.MULTILINE)
    if not uses or any(not SHA.fullmatch(revision) for revision in uses):
        problems.append("every release workflow action must be pinned to a full commit SHA")

    license_policy = policy.get("licenses", {})
    allowed = set(license_policy.get("allowed_spdx_ids", []))
    denied = set(license_policy.get("denied_spdx_ids", []))
    exceptions = {
        package
        for row in license_policy.get("migration_exceptions", [])
        if all(row.get(field) for field in ("owner", "reason", "expiry"))
        for package in row.get("packages", [])
    }
    for package in cargo_packages():
        expression = package.get("license")
        if not expression:
            if package["name"] not in exceptions:
                problems.append(f"{package['name']} has no SPDX license and no migration exception")
            continue
        tokens = license_tokens(expression.replace("/", " OR "))
        if tokens & denied:
            problems.append(f"{package['name']} uses denied license expression {expression}")
        unknown = tokens - allowed
        if unknown:
            problems.append(f"{package['name']} uses unreviewed SPDX identifiers: {sorted(unknown)}")
    return problems


def check_bundle(policy: dict, bundle: Path) -> list[str]:
    problems: list[str] = []
    for arch in policy["artifacts"]["architectures"]:
        archive = bundle / policy["artifacts"]["archive_pattern"].format(arch=arch)
        sbom_path = bundle / policy["sbom"]["file_pattern"].format(arch=arch)
        scan_path = bundle / policy["vulnerabilities"]["report_pattern"].format(arch=arch)
        checksum_path = bundle / policy["artifacts"]["checksum_pattern"].format(arch=arch)
        for path in (archive, sbom_path, scan_path):
            if not path.is_file() or path.stat().st_size == 0:
                problems.append(f"release bundle is missing {path.name}")
        if sbom_path.is_file():
            sbom = load_json(sbom_path)
            if sbom.get("spdxVersion") != "SPDX-2.3":
                problems.append(f"{sbom_path.name} is not SPDX 2.3")
            for row in sbom.get("files", []):
                checksums = row.get("checksums", [])
                if not any(item.get("algorithm") == "SHA256" and len(item.get("checksumValue", "")) == 64 for item in checksums):
                    problems.append(f"{sbom_path.name} has an unhashed file entry")
                    break
        if not checksum_path.is_file():
            problems.append(f"release bundle is missing {checksum_path.name}")
            continue
        declared = {}
        for line in checksum_path.read_text(encoding="utf-8").splitlines():
            match = re.fullmatch(r"([0-9a-f]{64})  (.+)", line)
            if match is None:
                problems.append(f"{checksum_path.name} contains a malformed checksum row")
                continue
            declared[match.group(2)] = match.group(1)
        for path in (archive, sbom_path, scan_path):
            if not path.is_file():
                continue
            actual = hashlib.sha256(path.read_bytes()).hexdigest()
            if declared.get(path.name) != actual:
                problems.append(f"{checksum_path.name} does not bind {path.name}")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--bundle", type=Path)
    parser.add_argument(
        "--bundle-only", type=Path, help="verify a release bundle without the source checks"
    )
    args = parser.parse_args()
    policy = load_json(POLICY_PATH)
    if args.bundle_only is not None:
        problems = check_bundle(policy, args.bundle_only)
        for problem in problems:
            print(f"release policy: {problem}", file=sys.stderr)
        print("release bundle holds" if not problems else "release bundle is invalid")
        return 1 if problems else 0
    problems = check_repository(policy)
    if args.bundle is not None:
        problems.extend(check_bundle(policy, args.bundle))
    for problem in problems:
        print(f"release policy: {problem}", file=sys.stderr)
    if problems:
        return 1
    print("release supply-chain policy holds")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (KeyError, TypeError, ValueError, json.JSONDecodeError, subprocess.CalledProcessError) as error:
        print(f"release policy failed: {error}", file=sys.stderr)
        raise SystemExit(1)
