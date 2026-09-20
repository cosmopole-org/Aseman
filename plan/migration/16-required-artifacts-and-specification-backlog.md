# Required Artifacts and Specification Backlog

## Why this register exists

The migration documents define architecture, sequencing, and acceptance, but implementation requires exact inventories, schemas, state machines, fixtures, and accepted decisions derived from the current code and product policy. An agent must create these artifacts at the stated phase; it must never infer them from names or this conversation.

Status values are `MISSING`, `DRAFT`, `ACCEPTED`, `GENERATED`, `VERIFIED`, and `RETIRED`. At the start of migration, all future artifacts are `MISSING`. Phase gates advance only when their required artifacts are accepted/verified.

## Phase 0: current truth and decisions

| ID | Required artifact | Intended path | Blocks |
|---|---|---|---|
| A001 | Repository/package/feature/dependency inventory | `docs/generated/current-workspace.*` | Phase 1 moves/deletions |
| A002 | Public, federation, guest, VMM-ingress, telemetry, and admin route inventory | `docs/generated/current-routes.*` | API extraction and parity |
| A003 | Environment/configuration/default/port/secret inventory | `docs/generated/current-configuration.*` | Typed config, deployment |
| A004 | Legacy RocksDB key prefixes, JSON shapes, indexes, QuestDB tables, ownership, and writers/readers | `docs/migration/legacy-data-map.md` | Capsule/PostgreSQL migration |
| A005 | Current action-to-handler-to-policy-to-storage/VMM call graph | `docs/migration/current-call-graph.*` | Use-case extraction/security |
| A006 | Current runtime capability/operation matrix | `docs/migration/current-runtime-matrix.md` | VMM parity/Nomad mappings |
| A007 | Existing CLI command and script behavior inventory | `docs/generated/current-cli-ops.md` | CLI/bootstrap replacement |
| A008 | Characterization/golden fixtures for supported behavior | `tests/characterization/` | Every rewrite/deletion |
| A009 | Baseline correctness, latency, throughput, memory, allocation, startup, and recovery report | `docs/migration/baseline.md` | Regression gates |
| A010 | Trust-boundary, threat, data-flow, failure, and state-authority models | `docs/architecture/` | Security/federation/providers |
| A011 | Removal ledger with owner/callers/target/expiry | `docs/migration/removal-ledger.md` | Every deletion gate |
| A012 | Accepted ADR set listed in `11-decisions-and-risks.md` | `docs/decisions/` | Dependent phases |
| A013 | Terminology and Caspar-to-Aseman mapping | `docs/glossary.md` | Naming/API/docs |
| A014 | Aseman control-plane HA, coordination, fencing, stable endpoint/identity ADR | `docs/decisions/` | Production node cluster |

## Phase 1: code and repository contracts

| ID | Required artifact | Intended path | Blocks |
|---|---|---|---|
| A101 | Root workspace/toolchain/lint/dependency policy | root manifests plus `docs/development/` | Reliable builds |
| A102 | Enforced crate/module dependency rules | `xtask` architecture configuration | Boundary acceptance |
| A103 | Typed `AsemanConfig` schema and legacy alias map | `contracts/config/` | All service startup/deploy |
| A104 | Domain type/state-machine catalog | generated docs from `aseman-domain` | Ports/use cases |
| A105 | Port catalog with semantic guarantees | generated docs from `aseman-ports` | Adapter/module implementation |
| A106 | Common-change and verification playbooks | `docs/development/common-changes/` | Agent/human implementation |

## Phase 2: module platform specifications

| ID | Required artifact | Intended path | Blocks |
|---|---|---|---|
| A201 | Complete `module.toml`/`.amod` JSON Schema | `contracts/module/` | Module packaging/install |
| A202 | Supervisor control protocol and compatibility/version policy | `contracts/module/control/` | Supervisor and CLI |
| A203 | Provider data-contract conventions, identity, health, errors, deadlines | `contracts/module/provider/` | All provider kinds |
| A204 | Artifact signature/trust/permission/secret model | ADR plus schemas | Secure module execution |
| A205 | Module lifecycle/routing-generation state machine | contracts/docs/tests | Activate/drain/rollback |
| A206 | Cluster placement/quorum/reconciliation rules | contracts/docs/tests | Cluster-wide install |
| A207 | Module conformance test kit and sample provider | `tests/contracts/module/` | Provider acceptance |
| A208 | Bootstrap snapshot schema, signing, recovery, and stale-state behavior | `contracts/module/bootstrap/` | Restart without active DB |

## Phase 3: capsule and storage specifications

| ID | Required artifact | Intended path | Blocks |
|---|---|---|---|
| A301 | Canonical capsule encoding, integrity, revision, tombstone, relationship semantics | `contracts/capsule/` | All persistence/migration |
| A302 | Typed query AST grammar and error/capability semantics | `contracts/capsule/query/` | Database independence |
| A303 | Required consistency/transaction/index capabilities per capsule kind | registry/schema | Provider activation |
| A304 | Complete core capsule-kind/schema registry | `contracts/capsule/kinds/` | Native mappings |
| A305 | PostgreSQL core table/index/constraint mapping plus guest database/role binding catalog | migrations plus generated docs | Default database |
| A306 | Per-creature database/namespace and role lifecycle; signed proxy authentication; portable multi-table/collection schema model; pool/query/cursor/cache/event/catalog isolation specification | contract and adversarial fixtures | Creature isolation |
| A307 | Telemetry/audit/finance/outbox/realtime storage semantics | schemas/contracts | Non-core storage classes |
| A308 | Legacy-to-capsule transform for every A004 entry | migration manifest/code | Backfill/cutover |
| A309 | Export/import/dual-write/checksum/read-compare/cutover/rollback protocol | runbook and tests | Provider switching |
| A310 | Storage provider conformance kit | `tests/contracts/storage/` | Third-party providers |

## Phase 4: security specifications

| ID | Required artifact | Intended path | Blocks |
|---|---|---|---|
| A401 | Identity/key/token/signature formats, canonical signed-request/challenge encoding, trust roots, audience/freshness/replay rules, rotation and revocation | `contracts/security/` | Authentication/federation/guest database proxy |
| A402 | Complete subject/action/resource/condition registry | `contracts/security/actions.*` | Universal authorization |
| A403 | Capability issue/delegate/attenuate/revoke state machine | contract/property tests | Child authority |
| A404 | Policy decision/error/explanation contract and conformance fixtures | `tests/contracts/policy/` | Security providers |
| A405 | Workload-program-creature resolution, signed request proof, and trusted database/role-binding proof | guest contract/adversarial tests | Guest data/API |
| A406 | Network/secret/default-deny enforcement matrix per runtime | policy/runtime matrix | Zero trust |

## Phases 5-6: VMM and worker specifications

| ID | Required artifact | Intended path | Blocks |
|---|---|---|---|
| A501 | Complete VMM OpenAPI, errors, idempotency, pagination, streams | `contracts/vmm/openapi.*` | Node/VMM split |
| A502 | Workload and operation lifecycle state machines | `contracts/vmm/states.*` | Reconciliation/providers |
| A503 | Desired/observed generation and conflict rules | VMM contract/tests | Crash recovery |
| A504 | VMM-backend protobuf contract/conformance kit | `contracts/vmm/backend/` | Native/Nomad backend swap |
| A505 | Native behavior parity matrix from A006 | test manifest | Embedded VMM deletion |
| A601 | Nomad mapping for each workload/runtime/capability | provider mapping docs/tests | Default VMM |
| A602 | Compact and HA server/client topology, ports, ACLs, certificates | deploy specs | Cluster operation |
| A603 | Worker-agent protocol and privilege/device model | contract/threat model | Firecracker/host work |
| A604 | Pause/resume/snapshot/terminal/log/usage semantics per runtime | runtime matrix | API truthfulness |
| A605 | Volume/snapshot portability and incompatible-move behavior | ADR/contract/runbook | Backend switching |
| A606 | Worker/server failure, cordon, drain, reschedule, and recovery scenarios | chaos fixtures | Production gate |
| A607 | Control-plane replica, coordination lease/fencing, failover, and duplicate-effect scenarios | contract/chaos fixtures | Master/control-plane HA |

## Phase 7: network, federation, and realtime specifications

| ID | Required artifact | Intended path | Blocks |
|---|---|---|---|
| A701 | Public HTTP OpenAPI and generated SDK compatibility policy | `contracts/public/` | HTTP default |
| A702 | Canonical gateway RPC for network modules | `contracts/gateway/` | Custom protocols |
| A703 | Listener broker bind/handoff/drain/failure semantics | contract/tests | Minimal-restart adapters |
| A704 | Node/workload descriptor schemas, signing, expiry, revocation, lookup | `contracts/federation/directory/` | Universal discovery |
| A705 | Federation envelope, replay, hop, dedupe, error, and signed-response contract | `contracts/federation/` | Cross-node actions |
| A706 | Trust bootstrap/rotation/revocation and partition behavior | ADR/runbooks/tests | Federation security |
| A707 | Event envelope, ordering, delivery, replay, retention, authorization | `contracts/realtime/` | Durable signalling |
| A708 | Realtime provider topology and capacity/failure model | ADR/deploy/runbook | Production realtime |

## Phase 8: finance and metering specifications

| ID | Required artifact | Intended path | Blocks |
|---|---|---|---|
| A801 | Normalized resource units, cumulative/delta rules, clock/skew/late-sample behavior | `contracts/metering/` | Correct charges |
| A802 | Pricing formula, rounding, currency/token precision, effective-version rules | `contracts/finance/pricing/` | Deterministic billing |
| A803 | Double-entry accounts, journal invariants, reservation/refund/settlement states | `contracts/finance/ledger/` | Ledger implementation |
| A804 | Consensus epoch/finality/checkpoint/provider-switch contract | `contracts/finance/consensus/` | Hashgraph modularity |
| A805 | Insufficient-funds/grace/pause/stop policy state machine | policy/contracts | Enforcement |
| A806 | Reconciliation and corrective-entry rules | runbooks/tests | Operational recovery |
| A807 | Golden usage-to-price-to-journal fixtures | `tests/contracts/finance/` | Cross-provider correctness |

## Phases 9-10: operations and release specifications

| ID | Required artifact | Intended path | Blocks |
|---|---|---|---|
| A901 | Component/image/port/volume/secret/certificate matrix | `deploy/README.md` | Compact/cluster install |
| A902 | Bootstrap/upgrade/backup/restore state machines and resumable journals | contracts/runbooks/tests | Fault-tolerant setup |
| A903 | CLI command/output/exit-code/idempotency compatibility catalog | generated CLI docs | Automation stability |
| A904 | Health/readiness/dependency semantics and support-bundle redaction rules | operations contract | Diagnostics |
| A905 | Dashboards, alerts, SLOs, error budgets, and capacity assumptions | `docs/operations/` | Production approval |
| A906 | Release signing, SBOM, provenance, vulnerability/license policy | CI/release docs | Distribution |
| A1001 | Full test/feature/platform matrix | CI manifest | Release gate |
| A1002 | Load/soak/chaos/failover/rollback scenarios and thresholds | test manifests | Production confidence |
| A1003 | Migration/canary decision criteria and abort thresholds | rollout runbook | Safe rollout |
| A1004 | Closed removal ledger and compatibility report | generated release evidence | Legacy deletion |
| A1005 | Final requirements traceability/evidence report | generated release evidence | Migration completion |

## Completeness rule

The plan is complete when it names every required artifact and gate; the implementation specification becomes complete incrementally when those artifacts are accepted. If code inspection reveals an unlisted protocol, state store, action, key family, feature, runtime behavior, or operational dependency, the agent must add it to this register and traceability matrix before modifying it.

No `MISSING` or `DRAFT` artifact may remain at a phase gate. `RETIRED` is valid only after its owning compatibility/removal obligation passes.
