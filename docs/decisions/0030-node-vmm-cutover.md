---
status: DECISION
owner: vmm
source_of_truth: this ADR, plan/migration/04-vmm-nomad-and-runtimes.md
last_verified_commit: eebb9c5
verification: cargo xtask full; aseman-vmm-backend-native live_native, live_docker, system
---

# ADR 0030: The node runs no runtimes; workloads reach it only through the guest API

## Status

Accepted 2026-09-22. It completes RL-013 for P5-03 through P5-06 and extends ADR 0029.

## Context

The node embedded the VMM: it linked every runtime engine (WasmEdge, QuickJS, bollard,
elpify, elpian, Modal, Firecracker), routed packets to them in process, and published
a `VmHost` through which a runtime reached node internals — the storage handle, the
global application, shell actions, the legacy transaction buffers. Plan 04 requires the
opposite: the node talks to a VMM only over A501, and a runtime reaches Aseman only
through the authenticated guest API.

Two facts shape how far this can go at once:
- A workload is a `core.workload` capsule, and the guest API resolves it from that
  record (A405). Those records exist only on the PostgreSQL provider (ADR 0026).
- Legacy observed VM instances are not desired state (ADR 0022); adopting them is an
  operator decision.

## Decision

1. **The node has no runtime.** `IVmm` is replaced by `IWorkloads`, which covers only
   what the node owns: the per-program signal listener, alarm wakes, HTTP ingress and
   custom gateway routes, resource locks, and the guest CRUD host actions. The packet
   router, the `VmHost` bridge, the runtime bootstrap, the identity-less callback
   protocol, the VM-context registry, the per-VM write buffers, and the dead VM gateway
   service are deleted. `caspar-vm-sdk` and `caspar-vm-plugins` are no longer node
   dependencies, so no runtime engine links into the node binary.
2. **Every VM operation is an A501 call.** `runVm`, `terminateVm`, `deleteVm`,
   `execVm`, `statusVm`, `copyToVm`, `copyFromVm`, `buildVmImage`, `vmEndpoints`, and
   `verifyProgramExecution` become VMM operations; `/programs/runEntity`, `stopEntity`,
   `deleteEntity`, `deploy`, ingress forwarding, chain messages, and the billing
   reaper follow the same path. A node without `ASEMAN_VMM_ENDPOINT` refuses them
   instead of running anything itself.
3. **A workload is a record, a key, and a desired state.** `/programs/runEntity`
   records `core.workload` (deterministic ID from program, entity, and instance),
   registers the workload's own Ed25519 key, and asks the VMM to create it. The
   private half travels once, as the write-only bootstrap credential. Lifecycle
   changes go through `SetDesiredWorkloadState`: desired state first, then a command
   keyed by generation.
4. **Host calls come back through the guest API.** A backend calls
   `POST /guest/v1/calls/{op}` (or `GET /guest/v1/artifacts/{digest}`) signed with the
   workload's key. The node authenticates the A401 proof, requires the signed action
   to be the call's registered A402 action, resolves the workload's creature and
   program from the trusted records, and stamps that identity on the host-call packet.
   Nothing the guest sends selects an identity. `stateOp` serves a runtime's
   creature-scoped state, confined to the caller's own creature (ADR 0021).
5. **Ordering.** The remote VMM requires `ASEMAN_CORE_STORAGE_PROVIDER=postgres`; the
   configuration refuses the combination otherwise. A deployment therefore does the
   Phase 3 cutover, then rolls out `aseman-vmm` with its backend, then adopts its
   legacy instances (`aseman-node vmm-handoff`), then runs a node of this version.
6. **Docker containers keep their gateway**, moved into the native backend. A
   container is still identified by its docker-network source IP and never declares
   its identity; the backend makes its calls as that container's workload.

## Consequences

- The node binary carries no runtime engine, and a guest cannot reach node internals:
  the only path is a signed request that names a registered action.
- A node on the legacy provider can still serve its API but cannot run programs. That
  is the intended pressure toward the Phase 3 cutover, and the runbook states the order.
- Legacy behavior kept deliberately: a runtime without HTTP ingress still receives a
  forwarded request as a signal and answers `202`, and cold-start queueing and the
  spawn debounce still apply to docker entities (now inside the backend).
- Two legacy defects surfaced while proving parity: LD-28 (an unprivileged backend
  cannot purge a sandbox a root container wrote into) and LD-29 (docker `execVm` read
  its output from a runtime that had already shut down, so it never returned any).
