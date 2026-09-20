---
status: DECISION
owner: vmm/security
source_of_truth: this ADR
last_verified_commit: 800df24076c7
verification: A603 privilege model and runtime conformance suite
---

# ADR 0010: Firecracker through the restricted worker agent

## Status

Accepted 2026-09-19.

## Decision

Firecracker host operations are implemented by `aseman-vmm-agent`, not by a
privileged Nomad task-driver plugin. Nomad schedules an unprivileged runner task; the
VMM backend supplies a signed, short-lived allocation/workload operation to the local
agent. The agent validates scheduler placement and Aseman identity, applies an
allowlisted machine/network/volume specification, and owns `/dev/kvm`, tap/network,
cgroup, jailer, snapshot, and process lifecycle operations.

The agent exposes no general shell, arbitrary file path, arbitrary device, or raw
Firecracker API. Paths derive from an agent-owned allocation root; network/device
profiles are administrator-declared. Requests are idempotent and auditable. A host can
disable the Firecracker capability independently.

This keeps scheduler integration replaceable and concentrates unavoidable host
privilege in one small, hardened component. It costs an additional service and local
protocol.

## Migration and rollback

The legacy embedded Firecracker controller is first wrapped by the VMM runtime
contract, then its privileged effects move operation-by-operation to the agent. The
native VMM provider can also call the same agent. Rollback selects the prior provider
only while the legacy path remains inside its compatibility window; it never grants
the node API process agent privileges.

Rejected: production `raw_exec`, a broadly privileged controller, and tying the only
Firecracker implementation to Nomad's plugin ABI.
