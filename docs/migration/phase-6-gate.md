---
status: ACCEPTED
owner: migration/phase-6
source_of_truth: plan/migration/09-migration-phases.md (Phase 6)
last_verified_commit: eebb9c5
verification: cargo xtask fast; live Nomad v2.0.7, PostgreSQL 16, Firecracker v1.17.0, and Docker
---

# Phase 6 exit gate

## Decision

**Accepted, with two items explicitly owned onward.** The Nomad provider exists and
passes the same A504 suite as the native one. Worker and replica loss are handled and
proven. Singleton work is fenced so it cannot execute twice. There is one scheduler, and
the node holds no worker-management role.

## Gate clauses

| Clause | Evidence |
|---|---|
| Native and Nomad providers pass the same contract suite | `check_backend` — the A504 kit the native backend passes — runs against `NomadBackend` in `tests/live_nomad.rs`, on a real cluster |
| Adding or removing a worker does not change Aseman federation identity | P6-03 asserts the workload's ID and generation before a cordon, after a drain, and after the return; the Aseman node ID lives in capsule state and nothing in the worker path touches it (ADR 0013, A602) |
| Losing a control replica does not change federation identity | P6-03A: replicas are interchangeable holders of a named lease; a replica address is never published as node identity |
| Fenced singleton work does not execute twice | `live_singleton_work_happens_on_one_replica_at_a_time`: four replicas, ten passes each, on real PostgreSQL, with an enter/leave log replayed to prove the depth never exceeds one. Eight replicas racing for one lease produce exactly one holder |
| No second scheduler, and no ambiguous OpenRaft worker-management role | Placement is Nomad's; Aseman keeps desired state. The node enrolls no workers. Coordination is the PostgreSQL lease, not a Raft leader (ADR 0012, ADR 0013) |

## Work items

| Item | Status | Record |
|---|---|---|
| Desired state as Nomad jobs and allocations | Delivered | P6-01, A601 |
| Compact and HA topology, ports, ACLs, certificates | Delivered | P6-02, A602 |
| Worker enroll, cordon, drain, removal | Delivered | P6-03, A606 |
| Replicated control plane, fenced leases, failover | Delivered | P6-03A, A607 |
| Docker/QEMU/runner mappings and hardened tasks | Delivered | P6-04 |
| Firecracker and pause/resume through the worker agent | Delivered for the protocol and privilege model | P6-05, A603, A604 |
| Workload identity, logs, events, allocation statistics | Delivered | P6-01 |
| Lost-worker and control-plane recovery | Delivered | P6-03, P6-03A |
| Volume portability and incompatible-move behavior | Delivered as rules and order | P6-06, A605 |
| Adopt or release legacy Modal handles | Operator action, unchanged | ADR 0022, P5-05 |

## Two things this phase refuses to pretend

**Egress.** A workload's policy is deny-by-default. Nomad's plain bridge hands it the
internet, so the backend refuses to place a deny-by-default workload there and requires
a CNI network that enforces it. The reference network is proven to deny egress by a
probe that reaches the internet when run unrestricted. Selective per-destination
allowances are refused rather than guessed.

**Booting a microVM.** This host has no `/dev/kvm`. The agent's protocol, privilege
model, grant checks, path confinement, and pause semantics are proven against the real
Firecracker binary — including that a host which cannot boot says so instead of
reporting a running machine. Booting a guest needs a KVM-capable host and an
administrator-supplied kernel, and is recorded as open rather than claimed.

## Owned by later phases

- **Runner images** for javascript, wasm, elpian, and elpify are declared per runtime by
  the operator; building the hardened runner is Phase 9 packaging (A007).
- **Executing a stateful move** needs a provider that can snapshot; the rules and the
  order are fixed (A605) so it is done correctly when one can.
- **Compose and systemd profiles** follow the bootstrap work in Phase 9 (plan 08);
  A602 fixes what they must produce.
- **Ending the OpenRaft path** is RL-012's deletion gate.
