#!/usr/bin/env python3
"""Check `contracts/deploy/topology.json` (A602) against the repository.

A deployment contract nobody checks is a wish. This asserts the parts of the topology
that the code actually decides: which binaries exist, that the A504 listener really is
loopback-only, that the guest API path is the one the contract publishes, and that no
service claiming no privileges quietly asks for the Docker socket or KVM.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CONTRACT = ROOT / "contracts/deploy/topology.json"

# service name -> the crate manifest that builds it, when Aseman builds it at all.
OWNED_BY = {
    "aseman-node": "node/Cargo.toml",
    "aseman-vmm": "apps/aseman-vmm/Cargo.toml",
    "aseman-vmm-backend": None,  # one of several backends; checked separately
    "aseman-vmm-agent": None,  # P6-05
    "nomad-server": None,  # the operator's (ADR 0002)
    "nomad-client": None,
    "postgres": None,
}

BACKEND_MAINS = [
    "modules/vmm-backend/native-legacy/src/main.rs",
    "modules/vmm-backend/nomad/src/main.rs",
]


def fail(problems: list[str], message: str) -> None:
    problems.append(message)


def check() -> list[str]:
    problems: list[str] = []
    contract = json.loads(CONTRACT.read_text(encoding="utf-8"))

    services = contract["services"]
    for name, manifest in OWNED_BY.items():
        if name not in services:
            fail(problems, f"{name} is deployed but the topology contract omits it")
        if manifest and not (ROOT / manifest).exists():
            fail(problems, f"{name} names {manifest}, which does not exist")
    for name in services:
        if name not in OWNED_BY:
            fail(problems, f"the contract names {name}, which nothing deploys")

    # The A504 listener is the trust boundary: it must refuse a non-loopback address.
    a504 = next(
        port
        for port in services["aseman-vmm-backend"]["ports"]
        if port["name"] == "a504"
    )
    if a504.get("bind") != "loopback":
        fail(problems, "the contract no longer says the A504 listener is loopback-only")
    for main in BACKEND_MAINS:
        source = (ROOT / main).read_text(encoding="utf-8")
        if "is_loopback()" not in source:
            fail(problems, f"{main} does not enforce the loopback-only A504 listener")

    # The guest API path the contract publishes is the one the contract crate serves.
    guest = next(
        port for port in services["aseman-node"]["ports"] if port["name"] == "guest"
    )
    guest_api = (ROOT / "crates/aseman-contracts/src/guest_api.rs").read_text(
        encoding="utf-8"
    )
    calls = re.search(r'CALLS_PATH: &str = "([^"]+)"', guest_api)
    if not calls:
        fail(problems, "CALLS_PATH is gone from the guest API contract")
    elif not calls.group(1).startswith(guest["path"] + "/"):
        fail(
            problems,
            f"the contract publishes {guest['path']} but the guest API serves {calls.group(1)}",
        )

    # A service that claims no privileges must not be asking for any.
    privileged = {
        "docker": re.compile(r"docker\.sock|bollard"),
        "kvm": re.compile(r"/dev/kvm"),
    }
    unprivileged = {
        "aseman-node": ["node/src"],
        "aseman-vmm": ["apps/aseman-vmm/src"],
    }
    for name, directories in unprivileged.items():
        if "none" not in services[name]["privileges"]:
            continue
        for directory in directories:
            for path in (ROOT / directory).rglob("*.rs"):
                text = path.read_text(encoding="utf-8", errors="replace")
                for kind, pattern in privileged.items():
                    if pattern.search(text):
                        fail(
                            problems,
                            f"{name} claims no privileges but {path.relative_to(ROOT)} "
                            f"reaches for {kind}",
                        )

    # Bring-up order must name only services the contract knows.
    for service in contract["ordering"]["bring_up"]:
        if service not in services:
            fail(problems, f"the bring-up order names unknown service {service}")

    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="accepted for symmetry with the generators; this script only checks",
    )
    parser.parse_args()
    problems = check()
    for problem in problems:
        print(f"deploy topology: {problem}", file=sys.stderr)
    if problems:
        return 1
    print("deploy topology contract holds")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
