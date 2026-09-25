---
status: ACCEPTED
owner: migration/phase-5
source_of_truth: plan/migration/09-migration-phases.md (Phase 5)
last_verified_commit: eebb9c5
verification: cargo xtask full; live native, docker, and end-to-end system tests against the real backend
---

# Phase 5 exit gate

## Decision

**Accepted.** The VMM is a service of its own. The node holds no runtime engine and no
second lifecycle path: every VM operation it performs is an A501 call, answered by the
VMM service over A504 by the native backend, which is the only process that links a
runtime plugin. Desired and observed state, operations, idempotency, and events live in
the VMM (A502, A503); the node keeps signals, ingress, routes, locks, and the guest API.

## Work items

| Item | Status | Record |
|---|---|---|
| Provider-neutral VMM facade and `modules/vmm-backend/native-legacy` | Delivered | P5-01, P5-02, A504 |
| HTTP/OpenAPI contract and generated client | Delivered | P5-01, A501 (`modules/vmm-http`) |
| Every node-to-runtime call replaced with VMM client operations | Delivered | P5-06, ADR 0030 |
| No VMM access to node globals, raw storage, shell actions, or finance | Delivered | P5-02, P5-06 (the backend reaches Aseman only through the guest API) |
| Lifecycle, logs, terminal, events, usage, operation state, reconciliation | Delivered | P5-01, P5-02, P5-07, A502, A503 |
| Adopt or explicitly stop every handoff-inventory instance | Delivered | P5-05, `docs/operations/vmm-handoff-runbook.md` |

## Gate clauses

| Clause | Evidence |
|---|---|
| The node binary has no runtime-engine dependency | `apps/aseman-node/Cargo.toml` names no `caspar-vm-*` crate; `cargo tree -p aseman-node` reaches no wasmedge, bollard, rquickjs, elpify, or elpian |
| No competing in-process VMM path | `IVmm`, the bridge, the packet router, the host bridge, `hostcall_global`, and bootstrap are deleted (P5-06); `IWorkloads` is the only workload surface and it calls `VmmClient` |
| Obsolete runtime globals, code, and configuration are deleted | The P5-06 deletion table; `NetworkConfig.docker_gateway_port` moved to the runtimes |
| The native provider passes parity | `docs/generated/vmm-native-parity.md`: 22 of the 27 runtime operations verified against the real engines, 46 node methods `deleted`, 5 `open` and owned by P6 |
| The native provider passes VMM conformance | `crates/aseman-ports/src/conformance/vmm.rs` runs against `NativeBackend` (`live_native`) and, through gRPC and HTTP, in `system` |
| Every handoff-inventory instance is adopted or explicitly stopped | `aseman-node vmm-handoff` plans, checks, and completes decisions bound to the export digest (P5-05) |

## Live proof

- `modules/vmm-backend/native-legacy/tests/system.rs`: a node client drives the VMM over
  mTLS with PostgreSQL stores and its background executor, observer, and reconciler; the
  VMM drives the native backend process over A504; the backend runs a real JavaScript
  runtime whose guest calls come back through the guest API. The test kills the backend,
  observes `Lost`, restarts it, and lets reconciliation converge, then deletes.
- `modules/vmm-backend/native-legacy/tests/live_docker.rs`: build, run, HTTP into the
  container, file copy, exec, stop, and delete on the real Docker daemon.
- `modules/vmm-backend/native-legacy/tests/live_native.rs`: the port conformance kit
  against the backend in-process.

## Legacy defects closed

- **LD-29:** docker `execVm` never returned output (a stream read from a shut-down
  runtime). Fixed in P5-03; the docker parity test asserts the output.
- **LD-30:** docker build output went to a node-wide stream nobody owned, and after the
  node's log store was deleted `/machines/readVmLogs` answered every request with an
  empty page. Fixed in P5-07: the endpoint reads the workload's A501 log stream.
- **LD-28:** an unprivileged backend cannot purge a sandbox a root container wrote into.
  Recorded, not hidden: the failure is reported, the runbook gives the deployment rule,
  and the privileged host work belongs to the P6 worker agent.

## Ordering constraint

A remote VMM requires the PostgreSQL cutover first: `aseman-config` refuses a remote VMM
under the legacy storage provider, because the VMM's stores and the node's workload
records must be the same database. ADR 0030 and both runbooks state the order — Phase 3
cutover, VMM rollout, handoff, then the new node.

## Owned by later phases

- **Placement, scheduling, and the worker topology** are P6: the 5 `open` parity rows
  (pause, resume, snapshot, migrate, volume attach) are the P6 runtime module's, as is
  the privileged host work LD-28 names.
- **Replacing `modules/vmm-backend/native-legacy/crates/caspar-vm-plugins` compile-time
  aggregation with signed runtime loading** is RL-014.
- **Public transports for VM operations** (terminal and log streaming to end users) are
  P7-01 and P7-04; the VMM already emits the events they carry.
