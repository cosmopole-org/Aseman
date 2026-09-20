# Agent Execution Guide

## Purpose

This folder is the complete migration context. An implementation agent must not rely on the conversation that created it. The repository and these documents are authoritative; chat history is not.

An agent may start Phase 0 immediately. It must not implement a later phase until that phase's incoming dependencies, blocking ADRs, and required specification artifacts are complete.

## Required reading order

For every new agent/session:

1. `README.md` for outcome and invariants.
2. `14-plan-integrity-and-traceability.md` for terms, call paths, state ownership, dependencies, and requirement IDs.
3. `09-migration-phases.md` for the active phase and gate.
4. `16-required-artifacts-and-specification-backlog.md` for required inputs/outputs.
5. The capability-specific design document (`02` through `08`, `12`, or `13`).
6. `10-verification-and-acceptance.md` and the relevant conformance requirements.
7. `11-decisions-and-risks.md` for accepted/open ADRs and hazards.
8. Current repository instructions (`AGENTS.md`) when Phase 1 creates them.

Until a root `AGENTS.md` exists, this guide and the migration index govern migration work.

## Source-to-target map

| Current source | Current responsibility | Target owner | Removal proof |
|---|---|---|---|
| `node/src/main.rs` | Configuration, composition, startup, lifecycle | `apps/aseman-node` composition root plus `aseman-config` | No business rules or direct environment reads remain in `main`. |
| `node/src/models/*` | Mixed domain, ports, packets, state, worker DTOs | `aseman-domain`, `aseman-ports`, `aseman-contracts` | No global model bucket; every type has one semantic owner. |
| `node/src/core/*` | Transactions, orchestration, globe, actor registry | `aseman-application` use cases plus selected domain/services | No service locator or driver imports in application/domain. |
| `node/src/shell/api/actions/*` | Routes, authorization, domain rules, persistence, VM and finance behavior | Application use cases plus gateway DTO adapters | Route handlers contain translation only; large action files deleted. |
| `node/src/drivers/storage.rs` and RocksDB transaction code | KV and time-series persistence | Capsule ports, PostgreSQL module, legacy RocksDB migration module | Node/application no longer imports RocksDB/QuestDB types. |
| `node/src/drivers/security.rs` | Signing and verification | Identity/crypto adapter plus policy/security module | All authorization passes one policy decision contract. |
| `node/src/drivers/signaler.rs` | In-process realtime callbacks | Realtime port and durable/in-memory modules | No authoritative process-local subscription state in production. |
| `node/src/drivers/network/client/*` | TCP/WS framing plus duplicated sessions | Network modules plus transport-neutral gateway/session application | Legacy adapters own framing only and expire on the removal ledger. |
| `node/src/drivers/network/federation/*` | Peer transport and routing | Federation HTTP module and application federation use cases | All envelope types share signed verification and replay rules. |
| `node/src/drivers/network/chain/hashgraph/*` | Hashgraph implementation/state | `modules/consensus/hashgraph` | Node finance depends only on consensus port/contract. |
| `node/src/drivers/cluster/*` | OpenRaft instance mesh | Removed or isolated optional role decided by ADR | No overlap with Nomad scheduling/worker authority. |
| `node/src/drivers/vmm/*` | Embedded VMM, host calls, runtime network and globals | `apps/aseman-vmm`, VMM-backend modules, worker agent, guest gateway | Node binary has no runtime dependencies/globals/direct calls. |
| `node/src/telemetry/*` | Profiling and node metrics | `aseman-observability` plus telemetry capsule/provider paths | Telemetry persistence uses capsules; exporters remain non-authoritative. |
| `cmd/casparctl/*` | Operator CLI | `apps/asemanctl` | Caspar binary/name remains only as expiring shim. |
| `vm-sdk/*`, `vms/*` | Compile-time runtime plugin ABI and implementations | Versioned runtime contracts and `modules/runtime/*` | No generated compile-time aggregation in node. |
| `client-cli/*`, `sdk/*` | Client tooling | Generated clients plus supported SDKs/examples | SDK behavior is generated/tested against OpenAPI contracts. |
| Root scripts and `Dockerfile` | Build/install/multi-process operations | `xtask/`, `deploy/`, bootstrap and separate images | Giant installer and multi-process image removed after parity. |
| `wiki/*` and root README | Current/legacy documentation | `docs/` current portal plus `docs/legacy/caspar` archive | No contradictory duplicated inventories. |
| `dist/*` | Checked-in binaries/runtime blobs | Signed releases and OCI registry | Source tree and history no longer receive rebuilt binaries. |

This table is a subsystem map, not permission for bulk moves. Phase 0 inventories individual callers, routes, keys, features, and tests before deletion.

## Work-unit procedure

Every implementation work unit follows this order:

1. Identify requirement ID, phase, design authority, and acceptance owner.
2. Confirm required artifact/ADR inputs are present and accepted.
3. Record current behavior with tests or fixtures before changing it.
4. Define the smallest independently reviewable change.
5. State dependency-boundary impact and state ownership.
6. Implement the replacement without making it authoritative prematurely.
7. Run targeted unit/contract tests, then the phase-required wider tests.
8. Exercise failure, cancellation, retry, restart, and rollback behavior.
9. Update generated contracts/docs/inventories.
10. Update the removal ledger and delete obsolete callers after parity.
11. Capture evidence in the work-unit record and phase gate.

## Work-unit record

Each issue/PR uses this template:

```markdown
Requirement: Rxx
Phase/work package: Px-yy
Current owner/path:
Target owner/path:
Inputs/accepted ADRs:
State authority affected:
Contract/schema change:
Migration/cutover:
Rollback:
Security/threat impact:
Performance/index/backpressure impact:
Tests and commands:
Generated docs/inventories:
Removal-ledger entries:
Evidence and known limitations:
```

## Recommended work packages

### Phase 0

- `P0-01`: Generate repository, route, configuration, key-space, runtime, dependency, and feature inventories.
- `P0-02`: Add characterization/golden tests for supported public and guest behavior.
- `P0-03`: Produce threat/state/failure/data-flow models.
- `P0-04`: Decide all blocking ADRs or explicitly block dependent phases.
- `P0-05`: Establish correctness, performance, resource, and reliability baselines.
- `P0-06`: Create removal ledger and requirement/artifact status dashboards.

### Phase 1

- `P1-01`: Add root virtual workspace/toolchain without moving behavior.
- `P1-02`: Add architecture checks, lint policy, `xtask`, and scoped documentation.
- `P1-03`: Centralize typed configuration while preserving legacy environment aliases.
- `P1-04`: Extract domain identifiers/value objects/state machines.
- `P1-05`: Extract narrow ports and application use cases one action family at a time.
- `P1-06`: Consolidate transport-neutral session behavior behind characterization tests.

### Phase 2

- `P2-01`: Freeze module manifest/control/data contract schemas and compatibility fixtures.
- `P2-02`: Implement trust store, artifact verification, cache, and bootstrap snapshot.
- `P2-03`: Implement supervisor lifecycle, health, permissions, routing generations, and rollback.
- `P2-04`: Implement cluster reconciliation and administrative CLI.
- `P2-05`: Ship a harmless sample provider and pass the full module conformance suite.

### Phase 3

- `P3-01`: Implement capsule envelope, definitions, query AST, revisions, and capability negotiation.
- `P3-02`: Implement native PostgreSQL core schemas/repositories.
- `P3-03`: Implement per-creature guest database/namespace provisioning, dedicated roles, multi-table/collection schema management, bounded pool isolation, and adversarial cross-database/catalog tests.
- `P3-04`: Implement telemetry/audit/finance/outbox/realtime native mappings.
- `P3-05`: Wrap legacy storage and implement canonical export/import.
- `P3-06`: Add dual write, semantic comparison, cutover, rollback, and delete node DB leakage.

### Phase 4

- `P4-01`: Implement identities, trust roots, key rotation, and revocation.
- `P4-02`: Implement action/resource registry and policy decision contract.
- `P4-03`: Implement capability issuance, attenuation, expiry, explanation, and descendant revocation.
- `P4-04`: Implement signed workload authentication plus trusted workload-to-creature database/role resolution in the guest proxy.
- `P4-05`: Enforce every registered action and run authorization property/adversarial tests.

### Phase 5

- `P5-01`: Freeze VMM OpenAPI and lifecycle/operation state machines.
- `P5-02`: Implement provider-neutral `aseman-vmm` service and node client.
- `P5-03`: Extract native backend incrementally by lifecycle operation.
- `P5-04`: Move host calls to guest gateway and remove node/global access.
- `P5-05`: Implement logs, terminal, events, usage, restart reconciliation, and parity suite.
- `P5-06`: Delete embedded VMM/runtime dependencies from the node.

### Phase 6

- `P6-01`: Implement Nomad job/allocation mapping and reconciliation.
- `P6-02`: Implement compact topology and workload identity.
- `P6-03`: Implement server/client cluster enrollment, cordon, drain, and loss recovery.
- `P6-03A`: Implement replicated Aseman control plane, stable endpoint/identity, coordination provider, fenced leases, and failover.
- `P6-04`: Implement Docker/QEMU/runner mappings and capability scheduling.
- `P6-05`: Implement worker agent and Firecracker/pause/resume/snapshot semantics.
- `P6-06`: Implement provider/volume portability rules and cluster chaos tests.

### Phase 7

- `P7-01`: Implement public HTTP/OpenAPI gateway and shared session path.
- `P7-02`: Implement signed node/workload descriptors and universal minimal identity resolution.
- `P7-03`: Implement signed federation envelopes, home routing, replay/idempotency, and destination authorization.
- `P7-04`: Implement durable realtime, outbox, replay, checkpoints, and federated relay.
- `P7-05`: Convert legacy TCP/WS to framing adapters, verify parity, then expire them.

### Phase 8

- `P8-01`: Extract pricing, ledger, consensus, metering, and enforcement ports.
- `P8-02`: Adapt Hashgraph and build financial epoch/checkpoint switching.
- `P8-03`: Implement normalized VMM usage history, cursors, deltas, and minute intervals.
- `P8-04`: Implement deterministic pricing and idempotent double-entry settlement.
- `P8-05`: Implement backfill, reconciliation, insufficient-funds policy, and failure tests.

### Phase 9

- `P9-01`: Complete CLI surfaces and stable structured output.
- `P9-02`: Build separate least-privilege images and compact profile.
- `P9-03`: Build clustered/systemd worker profiles and secrets/certificate flow.
- `P9-04`: Implement resumable bootstrap/upgrade/backup/restore/doctor/support bundle.
- `P9-05`: Publish generated documentation, SDKs, dashboards, alerts, and runbooks.
- `P9-06`: Publish signed artifacts externally and remove `dist/` binaries from source control.

### Phase 10

- `P10-01`: Close coverage, fuzz, property, compatibility, and supply-chain gates.
- `P10-02`: Execute load, soak, failure, chaos, backup/restore, and rollback matrices.
- `P10-03`: Run shadow/canary rollout and compare correctness/performance.
- `P10-04`: Complete every deletion gate and overdue compatibility removal.
- `P10-05`: Run final traceability, documentation, agent-comprehension, and release audit.

## Verification commands

Before `xtask` exists, use component-local commands and record their limits:

```text
(cd node && cargo fmt --all -- --check)
(cd node && cargo check --workspace --all-targets)
(cd node && cargo test --workspace --all-targets)
(cd cmd/casparctl && cargo test --all-targets)
```

Native RocksDB/runtime compilation is expensive; a timeout or interrupted build is not a passing result. Phase 1 replaces this ambiguity with targeted `cargo xtask check-fast --changed` and explicit full-suite CI.

After `xtask` exists, use the commands defined in `12-llm-readiness.md`. Never invent a passing result for a command that did not complete.

## Stop conditions

Stop and mark the work package blocked when:

- A required ADR or artifact is absent.
- Two documents assign conflicting state ownership.
- The change would weaken a stated consistency/security guarantee.
- Migration or rollback cannot be described and tested.
- A provider cannot advertise a required capability.
- Existing behavior is unknown and lacks characterization evidence.
- The task would cross into a later phase to make the current phase appear complete.

Record the blocker in the artifact/decision backlog. Do not fill architectural gaps with implicit assumptions.

## Definition of done

A work package is done only when code, tests, contracts, schemas, generated docs, migration, rollback, observability, security review, performance impact, and removal-ledger updates agree. A phase is done only when all work packages and both its replacement and deletion gates pass.
