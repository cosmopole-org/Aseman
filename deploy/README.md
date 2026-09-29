---
status: CURRENT
owner: packaging/operations
source_of_truth: contracts/deploy/topology.json
verification: python3 scripts/check_deploy_topology.py
---

# Deployment assets

This is the canonical ownership root for deployment profiles. The current executable
topology, ports, identities, certificates, tokens, and privileges are defined in
[`../contracts/deploy/topology.json`](../contracts/deploy/topology.json) and explained
in [`../docs/operations/topology.md`](../docs/operations/topology.md).

`images/` contains separate one-process definitions for the node, VMM, meter, and the
two VMM backends (Nomad and native). They run as uid/gid 65532, declare a process health check, request no Docker
socket or KVM device, and are intended to run with a read-only root. Their build
context is a staged release tree (`scripts/stage-release.sh OUT`, or an unpacked
`aseman-dist-<arch>.tgz` from a GitHub Release), e.g.
`docker build -f deploy/images/node.Dockerfile OUT`. Production builds must override
`RUNTIME_IMAGE` with the release's digest-pinned base and attach the signed SBOM and
provenance; a floating `latest` tag is never a release input.

`systemd/aseman-vmm-agent.service` is the host profile for the only privileged Aseman
process. Its A603 executable binds loopback only, requires an allowlisted mTLS client
certificate and a short-lived Ed25519-signed allocation grant, exposes no shell or raw
Firecracker API, uses a closed device policy with only `/dev/kvm`, and confines writes
to the allocation root and cgroup tree. Copy and edit `vmm-agent.example.json`; never
reuse its placeholder fingerprint.

`compose/compact.compose.yaml` is the executable compact orchestration profile. It
keeps A504 loopback-only by placing the independently running backend in the VMM
container's network namespace, starts services in A602 order, drops every capability,
uses read-only roots, and exposes only the public node endpoint plus host-local health
ports. `compose/compact.env.example` documents its non-secret inputs; bootstrap creates
the secret files outside the repository.

The VMM backend is one Compose profile, chosen at installation:
`asemanctl bootstrap --backend nomad` (the default) runs workloads on the operator's
Nomad; `--backend native --vm-types modal,…` runs the native backend's runtime plugins
instead. The native backend serves only the VM types that need no host device (Modal
and the in-process runtimes; Docker and Firecracker need the Docker socket or KVM,
which the compact topology never grants). Each enabled type takes its settings as
bootstrap parameters, e.g. `--modal-api-key-secret FILE --modal-app-name NAME`; secret
values are always files, copied into the configuration directory's
`secrets/runtimes/` and handed to the backend's user. Bootstrap writes the backend's
`native-backend.json` and records `ASEMAN_BACKEND` and `COMPOSE_PROFILES` in
`compact.env`. The native image fetches the WasmEdge library pinned in
`contracts/release/runtime-dependencies.json` and refuses any other archive.

`compose/cluster.compose.yaml` is the executable service composition for three control
replicas sharing one node identity and external PostgreSQL/Nomad. It publishes the
three replica endpoints on host-local ports for an operator-owned stable load balancer;
the load balancer and secret/certificate distribution deliberately remain outside the
profile because they are deployment authority, not application containers.

Nomad is operator-provided under ADR 0002 and must not be downloaded, bundled,
mirrored, or redistributed by these assets. The compact profile connects to the
operator's endpoint through `nomad-backend.json` and never creates or removes Nomad.

## Installing on a host

`scripts/install.sh` (also attached to every GitHub Release) installs the release
binaries into `/opt/aseman` and downloads the third-party runtime libraries they need
— WasmEdge for the native VMM backend, and Firecracker with `--with-firecracker` —
refusing any archive whose SHA-256 differs from `contracts/release/runtime-dependencies.json`.
Then run `asemanctl bootstrap --profile compact`.
