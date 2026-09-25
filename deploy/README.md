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

`images/` contains separate one-process definitions for the node, VMM, meter, and Nomad
backend. They run as uid/gid 65532, declare a process health check, request no Docker
socket or KVM device, and are intended to run with a read-only root. `build-dist.sh`
publishes each canonical binary they copy. Production builds must override
`RUNTIME_IMAGE` with the release's digest-pinned base and attach the signed SBOM and
provenance; a floating `latest` tag is never a release input.

`systemd/aseman-vmm-agent.service` is the host profile for the only privileged Aseman
process. Its A603 executable binds loopback only, requires an allowlisted mTLS client
certificate and a short-lived Ed25519-signed allocation grant, exposes no shell or raw
Firecracker API, uses a closed device policy with only `/dev/kvm`, and confines writes
to the allocation root and cgroup tree. Copy and edit `vmm-agent.example.json`; never
reuse its placeholder fingerprint.

Compact orchestration and clustered profiles remain gated. Nomad is operator-provided
under ADR 0002 and must not be downloaded, bundled, mirrored, or redistributed by
these assets.
