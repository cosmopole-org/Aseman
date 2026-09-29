---
status: CURRENT
owner: architecture
source_of_truth: the artifacts named in the table
verification: cargo xtask fast
---

# Artifact catalog

Code, contracts, and documents cite specifications by artifact ID (`A402` is the
action registry). This catalog names each one and where it lives. An artifact marked
*retired* was a migration record; it is in the git history before the migration
archive, and nothing current depends on it.

## Inventories and decisions

| ID | Artifact | Location |
|---|---|---|
| A001 | Repository/package/feature/dependency inventory | `docs/generated/current-workspace.*` |
| A002 | Public, federation, guest, VMM-ingress, telemetry, and admin route inventory | `docs/generated/current-routes.*` |
| A003 | Environment/configuration/default/port/secret inventory | `docs/generated/current-configuration.*` |
| A004 | Legacy RocksDB key prefixes, JSON shapes, indexes, QuestDB tables, ownership, and writers/readers | *retired* |
| A005 | Current action-to-handler-to-policy-to-storage/VMM call graph | `apps/aseman-node/src/actions/mod.rs` (the operation table) |
| A006 | Current runtime capability/operation matrix | `docs/generated/current-runtime-matrix.{json,md}` |
| A007 | Existing CLI command and script behavior inventory | `docs/generated/current-cli-ops.*` |
| A008 | Characterization/golden fixtures for supported behavior | `apps/aseman-node/src/actions/tests.rs`, `tests/contract-checks/` |
| A009 | Baseline correctness, latency, throughput, memory, allocation, startup, and recovery report | *retired* |
| A010 | Trust-boundary, threat, data-flow, failure, and state-authority models | `docs/architecture/` |
| A011 | Removal ledger with owner/callers/target/expiry | *retired* |
| A012 | Accepted ADR set | `docs/decisions/` |
| A013 | Terminology and Caspar-to-Aseman mapping | `docs/glossary.md` |
| A014 | Aseman control-plane HA, coordination, fencing, stable endpoint/identity ADR | `docs/decisions/0013-control-plane-ha-and-fencing.md` |

## Code and repository

| ID | Artifact | Location |
|---|---|---|
| A101 | Root workspace/toolchain/lint/dependency policy | `docs/development/`, `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` |
| A102 | Enforced crate/module dependency rules | `xtask`, `xtask/src/main.rs` |
| A103 | Typed `AsemanConfig` schema and legacy alias map | `contracts/config/`, `crates/aseman-config` |
| A104 | Domain type/state-machine catalog | `docs/generated/domain-catalog.md`, `crates/aseman-domain` |
| A105 | Port catalog with semantic guarantees | `docs/generated/port-catalog.md`, `crates/aseman-ports` |
| A106 | Common-change and verification playbooks | `docs/development/common-changes/` |

## Module platform

| ID | Artifact | Location |
|---|---|---|
| A201 | Complete `module.toml`/`.amod` JSON Schema | `contracts/module/module.schema.json`, `contracts/module/amod.schema.json` |
| A202 | Supervisor control protocol and compatibility/version policy | `contracts/module/control/v1/control.proto` |
| A203 | Provider data-contract conventions, identity, health, errors, deadlines | `contracts/module/provider/v1/provider.proto` |
| A204 | Artifact signature/trust/permission/secret model | `docs/decisions/0015-module-artifact-trust-and-execution.md` |
| A205 | Module lifecycle/routing-generation state machine | `contracts/module/lifecycle.md`, `contracts/module/lifecycle.schema.json` |
| A206 | Cluster placement/quorum/reconciliation rules | `contracts/module/placement.md`, `contracts/module/placement.schema.json` |
| A207 | Module conformance test kit and sample provider | `tests/contracts/module/`, `tests/contracts/module`, `modules/sample-provider` |
| A208 | Bootstrap snapshot schema, signing, recovery, and stale-state behavior | `contracts/module/bootstrap/` |

## Capsules and storage

| ID | Artifact | Location |
|---|---|---|
| A301 | Canonical capsule encoding, integrity, revision, tombstone, relationship semantics | `contracts/capsule/encoding.md` |
| A302 | Typed query AST grammar and error/capability semantics | `contracts/capsule/query/` |
| A303 | Required consistency/transaction/index capabilities per capsule kind | `contracts/capsule/capabilities.schema.json` |
| A304 | Complete core capsule-kind/schema registry | `contracts/capsule/kinds/` |
| A305 | PostgreSQL core table/index/constraint mapping plus guest database/role binding catalog | `contracts/storage/postgres/core-mapping.json` |
| A306 | Per-creature database/namespace and role lifecycle; signed proxy authentication; portable multi-table/collection schema model; pool/query/cursor/cache/event/catalog isolation specification | `contracts/capsule/guest/` |
| A307 | Telemetry/audit/finance/outbox/realtime storage semantics | `contracts/capsule/storage-class-semantics.json`, `docs/generated/postgres-storage-class-mapping.md` |
| A308 | Legacy-to-capsule transform for every A004 entry | `modules/storage/rocksdb` |
| A309 | Export/import/dual-write/checksum/read-compare/cutover/rollback protocol | `contracts/migration/protocol.md`, `docs/operations/storage-migration-runbook.md`, `tests/migration` |
| A310 | Storage provider conformance kit | `tests/contracts/storage/`, `tests/contracts/storage` |

## Security

| ID | Artifact | Location |
|---|---|---|
| A401 | Identity/key/token/signature formats, canonical signed-request/challenge encoding, trust roots, audience/freshness/replay rules, rotation and revocation | `contracts/security/identity-v1.md`, `contracts/security/vectors/identity-v1.json` |
| A402 | Complete subject/action/resource/condition registry | `contracts/security/actions.*`, `contracts/security/policy-v1.md`, `docs/generated/security-action-registry.md` |
| A403 | Capability issue/delegate/attenuate/revoke state machine | `contracts/security/policy-v1.md`, `tests/contracts/policy/decisions-v1.json` |
| A404 | Policy decision/error/explanation contract and conformance fixtures | `contracts/security/policy-v1.md`, `tests/contracts/policy/decisions-v1.json` |
| A405 | Workload-program-creature resolution, signed request proof, and trusted database/role-binding proof | `contracts/security/guest-v1.md` |
| A406 | Network/secret/default-deny enforcement matrix per runtime | `contracts/security/runtime-matrix-v1.md` |

## VMM and workers

| ID | Artifact | Location |
|---|---|---|
| A501 | Complete VMM OpenAPI, errors, idempotency, pagination, streams | `contracts/vmm/openapi.*` |
| A502 | Workload and operation lifecycle state machines | `contracts/vmm/states.*` |
| A503 | Desired/observed generation and conflict rules | `contracts/vmm/states.json` |
| A504 | VMM-backend protobuf contract/conformance kit | `contracts/vmm/backend/v1/backend.proto`, `modules/vmm-backend-grpc`, `tests/contracts/vmm-backend` |
| A505 | Native behavior parity matrix from A006 | `contracts/vmm/native-parity.json`, `docs/generated/vmm-native-parity.{json,md}`, `scripts/generate_vmm_parity.py` |
| A601 | Nomad mapping for each workload/runtime/capability | `contracts/vmm/nomad/mapping.json`, `modules/vmm-backend/nomad` |
| A602 | Compact and HA server/client topology, ports, ACLs, certificates | `contracts/deploy/topology.json`, `docs/operations/topology.md`, `deploy/compose/{compact,cluster}.compose.yaml`, `deploy/systemd/aseman-vmm-agent.service` |
| A603 | Worker-agent protocol and privilege/device model | `contracts/vmm/agent/agent-v1.md`, `apps/aseman-vmm-agent`, `deploy/systemd/aseman-vmm-agent.service`, `apps/aseman-vmm-agent/tests/live_firecracker.rs` |
| A604 | Pause/resume/snapshot/terminal/log/usage semantics per runtime | `docs/generated/vmm-native-parity.md` |
| A605 | Volume/snapshot portability and incompatible-move behavior | `docs/operations/stateful-workload-moves.md` |
| A606 | Worker/server failure, cordon, drain, reschedule, and recovery scenarios | `modules/vmm-backend/nomad/src/workers.rs`, `modules/vmm-backend/nomad/tests/live_workers.rs` |
| A607 | Control-plane replica, coordination lease/fencing, failover, and duplicate-effect scenarios | `crates/aseman-storage-providers/tests/coordination.rs` |

## Network, federation, and realtime

| ID | Artifact | Location |
|---|---|---|
| A701 | Public HTTP OpenAPI and generated SDK compatibility policy | `contracts/public/openapi.json`, `contracts/public/client-policy.json`, `contracts/public/events-v1.md` |
| A702 | Canonical gateway RPC for network modules | `contracts/gateway/v1/gateway.proto`, `contracts/gateway/protocol-compatibility.json`, `modules/network/gateway` |
| A703 | Listener broker bind/handoff/drain/failure semantics | `contracts/gateway/listener-broker-v1.md`, `modules/network/gateway` |
| A704 | Node/workload descriptor schemas, signing, expiry, revocation, lookup | `contracts/federation/directory/descriptors-v1.md` |
| A705 | Federation envelope, replay, hop, dedupe, error, and signed-response contract | `contracts/federation/envelope-v1.md`, `contracts/federation/http-v1.md` |
| A706 | Trust bootstrap/rotation/revocation and partition behavior | `contracts/federation/directory/descriptors-v1.md`, `docs/operations/federation-trust-and-partitions.md` |
| A707 | Event envelope, ordering, delivery, replay, retention, authorization | `contracts/realtime/events-v1.md`, `crates/aseman-storage-providers/tests/realtime.rs` |
| A708 | Realtime provider topology and capacity/failure model | `contracts/realtime/capacity-v1.json`, `crates/aseman-storage-providers/tests/realtime.rs`, `scripts/check_observability_policy.py`, `docs/operations/observability.md` |

## Finance and metering

| ID | Artifact | Location |
|---|---|---|
| A801 | Normalized resource units, cumulative/delta rules, clock/skew/late-sample behavior | `contracts/metering/metering-v1.md` |
| A802 | Pricing formula, rounding, currency/token precision, effective-version rules | `contracts/finance/pricing/`, `contracts/finance/pricing-v1.md` |
| A803 | Double-entry accounts, journal invariants, reservation/refund/settlement states | `contracts/finance/ledger/`, `contracts/finance/ledger-v1.md`, `crates/aseman-storage-providers/tests/finance.rs` |
| A804 | Consensus epoch/finality/checkpoint/provider-switch contract | `contracts/finance/consensus/`, `contracts/finance/consensus-v1.md`, `modules/consensus/hashgraph` |
| A805 | Insufficient-funds/grace/pause/stop policy state machine | `contracts/finance/enforcement-v1.md` |
| A806 | Reconciliation and corrective-entry rules | `docs/operations/finance-reconciliation.md` |
| A807 | Golden usage-to-price-to-journal fixtures | `tests/contracts/finance/golden-usage-to-journal.json`, `crates/aseman-domain/src/finance/golden.rs` |

## Operations and release

| ID | Artifact | Location |
|---|---|---|
| A901 | Component/image/port/volume/secret/certificate matrix | `deploy/README.md`, `contracts/deploy/topology.json`, `docs/operations/topology.md`, `deploy/images` |
| A902 | Bootstrap/upgrade/backup/restore state machines and resumable journals | `contracts/operations/{operation-journal,backup-manifest}.schema.json`, `docs/operations/backup-restore.md`, `apps/asemanctl/src/cli/ops.rs`, `scripts/check_operations_contracts.py` |
| A903 | CLI command/output/exit-code/idempotency compatibility catalog | `contracts/cli/command-v2.json`, `contracts/public/client-policy.json`, `scripts/check_cli_contract.py`, `scripts/generate_public_clients.py` |
| A904 | Health/readiness/dependency semantics and support-bundle redaction rules | `contracts/deploy/health-v1.md`, `contracts/operations/support-bundle-redaction.json` |
| A905 | Dashboards, alerts, SLOs, error budgets, and capacity assumptions | `docs/operations/`, `contracts/observability/policy-v1.json`, `crates/aseman-observability`, `apps/aseman-node/src/observability/server.rs` |
| A906 | Release signing, SBOM, provenance, vulnerability/license policy | `contracts/release/policy-v1.json`, `scripts/{generate_release_sbom,check_release_policy}.py`, `docs/operations/release-supply-chain.md` |
| A1001 | Full test/feature/platform matrix | `xtask/src/main.rs` |
| A1002 | Load/soak/chaos/failover/rollback scenarios and thresholds | `contracts/testing/operational-scenarios-v1.json`, `scripts/run_operational_scenarios.py`, `docs/generated/operational-reports/` |
| A1003 | Migration/canary decision criteria and abort thresholds | `contracts/deploy/rollout-policy.json`, `docs/operations/staged-rollout.md`, `scripts/check_rollout_policy.py` |
| A1004 | Closed removal ledger and compatibility report | *retired* |
| A1005 | Final requirements traceability/evidence report | *retired* |
