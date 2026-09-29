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
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CONTRACT = ROOT / "contracts/deploy/topology.json"

# service name -> the crate manifest that builds it, when Aseman builds it at all.
OWNED_BY = {
    "aseman-node": "apps/aseman-node/Cargo.toml",
    "aseman-vmm": "apps/aseman-vmm/Cargo.toml",
    "aseman-meter": "apps/aseman-meter/Cargo.toml",
    "aseman-vmm-backend": None,  # one of several backends; checked separately
    "aseman-vmm-agent": "apps/aseman-vmm-agent/Cargo.toml",
    "nomad-server": None,  # the operator's (ADR 0002)
    "nomad-client": None,
    "postgres": None,
}

BACKEND_MAINS = [
    "modules/vmm-backend/native/src/main.rs",
    "modules/vmm-backend/nomad/src/main.rs",
]

IMAGES = {
    "aseman-node": ("deploy/images/node.Dockerfile", "aseman-node"),
    "aseman-vmm": ("deploy/images/vmm.Dockerfile", "aseman-vmm"),
    "aseman-meter": ("deploy/images/meter.Dockerfile", "aseman-meter"),
    "aseman-vmm-backend": (
        "deploy/images/nomad-backend.Dockerfile",
        "aseman-vmm-backend-nomad",
    ),
    "aseman-vmm-backend-native": (
        "deploy/images/native-backend.Dockerfile",
        "aseman-vmm-backend-native",
    ),
}

NATIVE_IMAGE = "deploy/images/native-backend.Dockerfile"
RUNTIME_DEPENDENCIES = ROOT / "contracts/release/runtime-dependencies.json"

COMPACT_PROFILE = "deploy/compose/compact.compose.yaml"
CLUSTER_PROFILE = "deploy/compose/cluster.compose.yaml"


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

    # Packaging consumes these exact executable names. Checking only the package name
    # let a `runner` target masquerade as `aseman-node` until image construction.
    required_bins = {
        "apps/aseman-node/Cargo.toml": {"aseman-node"},
        "apps/aseman-keygen/Cargo.toml": {"aseman-keygen"},
    }
    # ADR 0004's window is closed: the Aseman alias executables must not return.
    for manifest in ["apps/aseman-node/Cargo.toml", "apps/aseman-keygen/Cargo.toml", "apps/asemanctl/Cargo.toml"]:
        text = (ROOT / manifest).read_text(encoding="utf-8")
        for alias in ["caspar-node", "caspar-keygen", "asemanctl"]:
            if f'name = "{alias}"' in text:
                fail(problems, f"{manifest} still packages the retired alias {alias}")
    for manifest, expected in required_bins.items():
        data = tomllib.loads((ROOT / manifest).read_text(encoding="utf-8"))
        actual = {binary["name"] for binary in data.get("bin", [])}
        missing = sorted(expected - actual)
        if missing:
            fail(problems, f"{manifest} is missing packaged binary targets: {', '.join(missing)}")

    # asemanctl is Cargo's implicit `src/main.rs` target; keep that canonical entry
    # point beside the explicitly catalogued compatibility alias.
    if not (ROOT / "apps/asemanctl/src/main.rs").exists():
        fail(problems, "apps/asemanctl is missing the canonical asemanctl src/main.rs")
    for name in services:
        if name not in OWNED_BY:
            fail(problems, f"the contract names {name}, which nothing deploys")

    # Each unprivileged Aseman service is a separate, single-process, non-root image.
    # The privileged agent deliberately has no image until its authenticated server
    # executable exists; inventing a container for a library would be false evidence.
    build_script = (ROOT / "scripts" / "stage-release.sh").read_text(encoding="utf-8")
    for service, (dockerfile, binary) in IMAGES.items():
        path = ROOT / dockerfile
        if not path.exists():
            fail(problems, f"{service} has no separate image at {dockerfile}")
            continue
        source = path.read_text(encoding="utf-8")
        for required in ["USER 65532:65532", "HEALTHCHECK", f'ENTRYPOINT ["/usr/local/bin/{binary}"]']:
            if required not in source:
                fail(problems, f"{dockerfile} is missing {required}")
        for forbidden in ["docker.sock", "/dev/kvm"]:
            if forbidden in source:
                fail(problems, f"{dockerfile} asks for forbidden privilege {forbidden}")
        if binary not in build_script:
            fail(problems, f"stage-release.sh does not stage {binary} for {dockerfile}")

    # The native image's WasmEdge pin is the release contract's (A906), not a copy.
    wasmedge = json.loads(RUNTIME_DEPENDENCIES.read_text(encoding="utf-8"))["dependencies"]["wasmedge"]
    native = (ROOT / NATIVE_IMAGE).read_text(encoding="utf-8")
    amd64 = wasmedge["archives"]["amd64"]
    for label, value in [("URL", amd64["url"]), ("SHA-256", amd64["sha256"])]:
        if value not in native:
            fail(problems, f"{NATIVE_IMAGE} does not pin the contract's WasmEdge {label}")
    if "sha256sum --check" not in native:
        fail(problems, f"{NATIVE_IMAGE} does not verify the WasmEdge archive")

    agent_unit = ROOT / "deploy/systemd/aseman-vmm-agent.service"
    if not agent_unit.exists():
        fail(problems, "the host-profile agent has no systemd unit")
    else:
        unit = agent_unit.read_text(encoding="utf-8")
        for required in [
            "ExecStart=/usr/local/bin/aseman-vmm-agent",
            "DevicePolicy=closed",
            "DeviceAllow=/dev/kvm rw",
            "ProtectSystem=strict",
        ]:
            if required not in unit:
                fail(problems, f"the agent systemd unit is missing {required}")
        if "docker.sock" in unit:
            fail(problems, "the agent systemd unit must not receive the Docker socket")
    if "aseman-vmm-agent" not in build_script:
        fail(problems, "stage-release.sh does not stage the host agent executable")

    compact_path = ROOT / COMPACT_PROFILE
    if not compact_path.exists():
        fail(problems, f"the compact topology has no executable profile at {COMPACT_PROFILE}")
    else:
        compact = compact_path.read_text(encoding="utf-8")
        for service in ["postgres", "vmm", "nomad-backend", "native-backend", "node", "meter"]:
            if not re.search(rf"^  {re.escape(service)}:$", compact, re.MULTILINE):
                fail(problems, f"{COMPACT_PROFILE} omits the {service} service")
        # The backend is one profile: each backend service must declare its own.
        for service, profile in [("nomad-backend", "nomad"), ("native-backend", "native")]:
            block = re.search(rf"^  {service}:\n((?:    .*\n|\n)*)", compact, re.MULTILINE)
            if block is None or f"profiles: [{profile}]" not in block.group(1):
                fail(problems, f"{COMPACT_PROFILE} does not put {service} in the {profile} profile")
        for required in [
            "network_mode: service:vmm",
            "127.0.0.1:9090",
            "ASEMAN_CORE_STORAGE_PROVIDER: postgres",
            "ASEMAN_SIGNAL_LOG_PROVIDER: postgres",
            "cap_drop: [ALL]",
            "read_only: true",
        ]:
            if required not in compact:
                fail(problems, f"{COMPACT_PROFILE} is missing {required}")
        for forbidden in ["/var/run/docker.sock", "/dev/kvm", "image: nomad"]:
            if forbidden in compact:
                fail(problems, f"{COMPACT_PROFILE} includes forbidden compact input {forbidden}")
        # PostgreSQL is reachable by the node and VMM only; the VMM network namespace,
        # which the Nomad backend shares, needs the routed scheduler network.
        blocks = re.split(r"^  (?=[a-z0-9-]+:$)", compact, flags=re.MULTILINE)
        service_block = {block.split(":", 1)[0]: block for block in blocks[1:]}
        if "scheduler" in service_block.get("postgres", ""):
            fail(problems, f"{COMPACT_PROFILE} routes PostgreSQL onto the scheduler network")
        if "scheduler" not in service_block.get("vmm", ""):
            fail(problems, f"{COMPACT_PROFILE} leaves the VMM/Nomad backend namespace without a route to Nomad")
        # Published ports need a routed network: Docker publishes none for a container
        # that is only on internal networks.
        if not re.search(r"networks: \[[^\]]*\bpublic\b", service_block.get("node", "")):
            fail(problems, f"{COMPACT_PROFILE} publishes node ports without a routed network")

    cluster_path = ROOT / CLUSTER_PROFILE
    if not cluster_path.exists():
        fail(problems, f"the cluster topology has no executable profile at {CLUSTER_PROFILE}")
    else:
        cluster = cluster_path.read_text(encoding="utf-8")
        for service in ["vmm", "nomad-backend", "node-1", "node-2", "node-3", "meter"]:
            if not re.search(rf"^  {re.escape(service)}:$", cluster, re.MULTILINE):
                fail(problems, f"{CLUSTER_PROFILE} omits the {service} service")
        for required in [
            "network_mode: service:vmm",
            "ASEMAN_CORE_STORAGE_PROVIDER: postgres",
            "ASEMAN_SIGNAL_LOG_PROVIDER: postgres",
            "cap_drop: [ALL]",
            "read_only: true",
        ]:
            if required not in cluster:
                fail(problems, f"{CLUSTER_PROFILE} is missing {required}")
        for forbidden in ["/var/run/docker.sock", "/dev/kvm", "image: nomad", "postgres:"]:
            if forbidden in cluster:
                fail(problems, f"{CLUSTER_PROFILE} includes operator-owned input {forbidden}")

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
        "aseman-node": ["apps/aseman-node/src"],
        "aseman-vmm": ["apps/aseman-vmm/src"],
        "aseman-meter": ["apps/aseman-meter/src"],
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
