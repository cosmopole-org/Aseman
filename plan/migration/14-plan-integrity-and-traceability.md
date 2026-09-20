# Plan Integrity and Requirements Traceability

## Review result

The plan is internally coherent after the corrections recorded in this review and covers the stated product requirements. It is an architecture and migration specification, not proof that the current implementation satisfies them.

Execution is conditionally valid:

- Phase 0 must resolve the blocking ADRs and establish characterization/performance baselines.
- Every later phase must satisfy its replacement and deletion gates.
- A requirement is complete only when its implementation, migration, rollback, tests, documentation, and operational controls pass the acceptance criteria.
- Nomad cannot be described as unconditionally open-source; making it the shipped default remains subject to the documented licensing decision.

## Canonical terms

| Term | Meaning |
|---|---|
| Aseman node | The externally visible federated control-plane identity; internally it may be a cluster. |
| Master/control plane | Aseman control services plus Nomad servers for the node's internal worker cluster. |
| Worker | A physical/virtual host running a Nomad client and, when required, `aseman-vmm-agent`. |
| Workload/VM | A managed executable instance; it may be a container, microVM, WASM runtime, or other provider type. |
| Creature | The owner/security/data-sharing scope for programs and their workloads. |
| Program | A deployable workload definition owned by a creature. |
| Port | An in-process application-facing behavioral interface. |
| Adapter | In-process translation implementing a port or converting wire/domain values. |
| Module | A signed independently installed process/OCI artifact. |
| Provider | A module implementing one replaceable platform capability. |
| Runtime driver | VMM-side implementation controlling a workload technology. |
| Capsule | Portable logical persistence envelope; not necessarily the physical row/document/key representation. |
| Federation | Aseman-level discovery and authorized operations between independently administered nodes. |

## Authoritative call paths

```text
client/custom protocol
  -> network module
  -> canonical gateway contract
  -> aseman-node application use case
  -> policy decision
  -> typed port
  -> adapter/provider contract

aseman-node workload command
  -> node-facing VMM HTTP contract
  -> aseman-vmm common control logic
  -> VMM-backend module (Nomad/native/future)
  -> runtime or worker agent

workload guest call
  -> authenticated guest gateway
  -> resolved workload/program/creature identity
  -> policy decision
  -> trusted creature database/role binding
  -> capsule/schema application port
  -> selected provider under the dedicated role
```

No alternate authoritative call path is permitted after its migration phase.

## State ownership integrity

| State | Authority | Replicas/caches must not become authority |
|---|---|---|
| Users, creatures, programs, policy, desired workloads | Aseman application through capsule storage | VMM jobs, gateway caches |
| Observed allocation/runtime state | Active VMM endpoint/backend | Aseman's last-observed projection |
| Physical database representation and provider-native roles | Selected storage provider | Capsule export, bootstrap snapshots, caller-selected role/database |
| Logical schema and portable data | Capsule definitions/envelopes | Provider-specific mappings |
| Creature guest-database/role binding | Aseman application through capsule storage | Workload request fields, connection-pool residue |
| Workload placement | Nomad default backend or selected VMM provider | Aseman application |
| Federation home record | Workload's home Aseman node | Remote discovery caches |
| Wallet/ledger journal | Finance ledger provider under Aseman rules | Consensus transport, meter |
| Financial finality/order | Active consensus provider for its epoch | Pricing or VMM modules |
| Raw/normalized usage | Metering pipeline from signed VMM samples | Pricing projections |
| Module desired state | Signed module-registry capsules | Local bootstrap snapshot |
| Realtime delivery offsets | Active realtime provider plus capsule checkpoints | In-memory subscribers |
| Singleton control work/leadership | Active fenced `CoordinationPort` lease | Process liveness or network location |

## Phase dependency graph

```text
Phase 0 specifications, ADRs, baselines
  -> Phase 1 workspace/domain/ports/config boundaries
    -> Phase 2 module runtime and contract harnesses
      -> Phase 3 capsule persistence and PostgreSQL
        -> Phase 4 identity, capabilities, guest gateway
          -> Phase 5 extracted VMM HTTP service
            -> Phase 6 Nomad backend and worker topology
          -> Phase 7 HTTP/federation/realtime
            -> Phase 8 metering/finance (also requires Phases 3 and 5)
              -> Phase 9 packaging/CLI/bootstrap/operations
                -> Phase 10 hardening, canary, deletion, release
```

Phase numbering is the default execution order. Independent preparation may run in parallel, but a phase cannot cross its gate until every incoming dependency is complete.

## Requirements traceability

| ID | Requirement | Design authority | Delivery phase | Acceptance authority |
|---|---|---|---|---|
| R01 | Fine-grained modular, decoupled Rust architecture | `01`, `13` | 1 | `10` Architecture/Clean code |
| R02 | Runtime-installable signed provider modules through CLI | `02` | 2, 9 | `10` Architecture/Operations |
| R03 | VMM out of the node behind standard HTTP | `04` | 5 | `10` VMM |
| R04 | Native, Nomad, and future VMM replaceability | `04` | 5, 6 | `10` VMM |
| R05 | Docker, Firecracker, WASM, JavaScript, Elpian/Elpify and extensible runtimes | `04` | 5, 6 | VMM conformance/runtime matrix |
| R06 | Compact single-host and scalable master-worker modes | `04`, `08` | 6, 9 | `10` VMM/Operations |
| R07 | HTTP default plus custom client/federation protocols | `02`, `06` | 7 | `10` Network |
| R08 | Universal authenticated VM identity resolution with home-node address, ID, and public key | `06` | 7 | `10` Network |
| R09 | Authorized cross-node VM operations | `05`, `06` | 4, 7 | `10` Security/Network |
| R10 | PostgreSQL default and database independence | `03` | 3 | `10` Capsule storage |
| R11 | Universal capsule persistence for every storage class | `03` | 3 | `10` Capsule storage |
| R12 | Separate native SQL table/collection per core entity kind | `03` | 3 | `10` Capsule storage |
| R13 | One isolated guest database/namespace and role per creature, multi-table/collection control, shared within creature and isolated across creatures | `03`, `05`, ADR 0001 | 3, 4 | `10` Capsule/Security |
| R14 | Extensible security and zero-trust workloads | `05` | 4 | `10` Security |
| R15 | Attenuated administrator/parent-to-child rights | `05` | 4 | `10` Security |
| R16 | Extensible durable realtime signalling | `06` | 7 | `10` Network/Realtime |
| R17 | Modular finance and replaceable consensus, Hashgraph initially | `07` | 8 | `10` Finance |
| R18 | Actual per-minute resource metering and wallet settlement | `07` | 8 | `10` Finance |
| R19 | Fault-tolerant containers, installer, upgrade, backup, restore | `08` | 9 | `10` Operations |
| R20 | Comprehensive administrative CLI | `02`, `03`, `08` | 2-9 | CLI/E2E suites |
| R21 | LLM-readable architecture, docs, and workflows | `12` | all, 9-10 | `10` Agent comprehension |
| R22 | Dead/duplicate removal, standard hierarchy, algorithm quality | `13` | all, 10 | `10` Clean code |
| R23 | Caspar-to-Aseman compatibility and eventual cleanup | `01`, `13` | 1, 10 | Removal ledger/release gate |
| R24 | Standalone agent execution without conversation context | `12`, `15`, `16` | all | `10` Agent comprehension/artifact gates |
| R25 | One stable node identity across replicated control plane and scalable workers | `01`, `04`, `08` | 6, 9 | `10` VMM/Operations/chaos |

Document numbers refer to the numeric files in this folder. Every requirement has design, delivery, and acceptance ownership; none is satisfied by documentation alone.

## Blocking decision gates

| Decision | Must be resolved before | Failure behavior |
|---|---|---|
| Nomad license/distribution acceptance | ADR 0002 | Provider integration only; no redistribution without separate legal approval. |
| Module data/control RPC and version policy | ADR 0003 | Protobuf/gRPC plus explicit major/capability negotiation. |
| Capsule canonical encoding/integrity | ADR 0005 | Deterministic CBOR and versioned SHA-256 preimage. |
| Consistency requirements by capsule kind | ADR 0006 | Reject providers with unknown/weaker guarantees. |
| PostgreSQL versions/extensions | ADR 0007 | Majors 17/18; extensions optional. |
| Public node/workload identity and trust roots | ADR 0009 | Typed IDs, Ed25519, explicit enrolled roots. |
| Initial policy engine | ADR 0008 | Typed Rust evaluator behind provider port. |
| Firecracker driver versus agent | ADR 0010 | Restricted worker agent. |
| Stateful workload portability | ADR 0011 | Declared tiers; no implicit live migration. |
| Durable realtime default/topology | ADR 0014 | PostgreSQL durable provider; memory is non-production. |
| OpenRaft retained role or removal | ADR 0012 | Remove after fenced coordination/state migration. |
| Aseman control-plane coordination and fencing | ADR 0013 | Stable endpoint and PostgreSQL fencing-token leases. |
| Caspar compatibility duration | ADR 0004 | Two minor releases and at least 180 days. |

The ADR list in `11-decisions-and-risks.md` is the authoritative queue. Each ADR records decision, alternatives, consequences, phase gate, migration, rollback, and review date.

## Cross-cutting proof obligations

Every implemented requirement must prove:

1. Functional correctness and negative authorization behavior.
2. Contract compatibility and capability negotiation.
3. Idempotency, retry, cancellation, deadline, and shutdown semantics.
4. State authority and consistency under process/network failure.
5. Migration and rollback without silent loss or weaker guarantees.
6. Observability sufficient to diagnose partial completion.
7. Bounded resource use, indexing, backpressure, and benchmark behavior.
8. Documentation and machine-readable inventory freshness.
9. Removal of the superseded implementation after the compatibility window.

## Plan-change integrity rules

A change to this plan must update all affected layers:

- Invariant and outcome in `README.md`.
- Design document owning the capability.
- Phase work and gate in `09`.
- Acceptance criteria/tests in `10`.
- Decision/risk record in `11` when tradeoffs change.
- Traceability row and dependency/decision gate in this document.
- Removal ledger obligation when an existing behavior is replaced.
- Required-artifact register and agent work package when implementation inputs or sequence change.

CI for the plan/documentation should verify links, balanced code fences, unique numeric document IDs, requirement row ownership, referenced phase/acceptance sections, external reference reachability, and prohibited stale terminology.

## Review evidence

At this review:

- All local Markdown links in `plan/migration/` resolve.
- All fenced code blocks are balanced.
- Numeric document names are unique and sequential.
- All cited external technical references return successful HTTP responses.
- VMM facade versus backend-provider ownership has been clarified.
- Module installation, protocol, cluster rollout, and bootstrap recovery semantics have been persisted in the plan.
- Storage bootstrap state is explicitly a derived signed cache, preserving capsules as the authoritative persistence model.
- The requirements above have design, phase, and acceptance mappings.
- The source-to-target map, work-package sequence, and required-artifact backlog make missing implementation inputs explicit without relying on session context.

These mechanical checks must be automated before implementation work makes the plan a moving artifact.
