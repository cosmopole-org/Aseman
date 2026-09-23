---
status: CURRENT
owner: migration
source_of_truth: plan/migration/16-required-artifacts-and-specification-backlog.md and accepted artifacts
last_verified_commit: 800df24076c7
verification: docs/migration/phase-0-gate.md and recorded generator/test commands
---

# Migration artifact status

This dashboard records implementation evidence. The required artifact definitions and
phase blockers remain authoritative in `plan/migration/16-required-artifacts-and-specification-backlog.md`.

## Phase 0

| ID | Status | Evidence | Note |
|---|---|---|---|
| A001 | VERIFIED | `docs/generated/current-workspace.json`, `docs/generated/current-workspace.md` | Reproducible package/feature/dependency/lockfile inventory. |
| A002 | VERIFIED | `docs/generated/current-routes.{json,md}` | All observed shell, HTTP, guest, telemetry, and administrative surfaces feed A008/A011. |
| A003 | VERIFIED | `docs/generated/current-configuration.{json,md}` | 115 keys and their reads/declarations/deployment writes feed the exhaustive child ledger. |
| A004 | VERIFIED | `docs/migration/legacy-data-map.md`, `docs/generated/current-storage-access.json` | Physical layouts, key candidates, access sites, QuestDB, Hashgraph, and OpenRaft state are migration inputs; unknown runtime payloads must fail A308 review rather than be guessed. Corrected 2026-09-21: the scanner truncated files at the first `#[cfg(test)]` and missed most of `creature/finance.rs`. It now blanks only test items (292 direct accesses and 358 candidates, up from 243 and 306). |
| A005 | VERIFIED | `docs/migration/current-call-graph.md`, `docs/generated/current-call-graph.json` | All 77 registered actions map to handler, guard, and direct effects; target owners are in the support manifest. |
| A006 | VERIFIED | `docs/generated/current-runtime-matrix.{json,md}` | Runtime overrides/defaults are frozen and classified for target modules. |
| A007 | VERIFIED | `docs/generated/current-cli-ops.{json,md}` | CLI/scripts are frozen, classified, and governed by ADR 0004. |
| A008 | VERIFIED | `tests/characterization/`, `crates/aseman-contracts/src/legacy_*.rs`, `node/src/shell/storage_http.rs` | Golden surface and reviewed support/target-owner manifest plus the recorded 401-node/33-CLI baseline; migrated pure cases now run in clean crates and deeper semantics remain mandatory at each replacement gate. |
| A009 | ACCEPTED | `docs/migration/baseline.md`, `docs/generated/current-quality-baseline.{json,md}` | Correctness/resource measurements, static ratchets, known harness gaps, and mandatory first targets recorded. |
| A010 | ACCEPTED | `docs/architecture/` | State authority, trust boundaries, flows, failures, and threats recorded with phase-owned proof obligations. |
| A011 | ACCEPTED | `docs/migration/removal-ledger.md`, `docs/generated/removal-ledger-children.{json,md}` | 19 parent rows (RL-019 added by ADR 0019) and the generated configuration/storage/protocol/artifact child rows. |
| A012 | ACCEPTED | `docs/decisions/0001-*.md` through `0014-*.md` | Entire Phase 0 blocking ADR set resolved. |
| A013 | ACCEPTED | `docs/glossary.md` | Canonical terminology and Caspar-to-Aseman map established. |
| A014 | ACCEPTED | `docs/decisions/0013-control-plane-ha-and-fencing.md` | Replica topology, stable identity/endpoint, lease semantics, and fencing contract accepted. |

Phase 0 gate evidence is recorded in `docs/migration/phase-0-gate.md`.

## Phase 1

| ID | Status | Evidence | Note |
|---|---|---|---|
| A101 | ACCEPTED | root `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `.cargo/`, `docs/development/` | One workspace, pinned toolchain, canonical binary, lint/dependency policy. |
| A102 | ACCEPTED | `xtask/src/main.rs`, `cargo xtask arch` | Protected dependency direction plus deterministic fast/full gates. |
| A103 | ACCEPTED | `contracts/config/`, `crates/aseman-config` | Typed configuration and exhaustive 115-key compatibility map with fail-closed conflicts. |
| A104 | VERIFIED | `docs/generated/domain-catalog.md`, `crates/aseman-domain` | Generated catalog backed by pure-domain unit tests. |
| A105 | VERIFIED | `docs/generated/port-catalog.md`, `crates/aseman-ports` | Generated narrow behavioral-port catalog. |
| A106 | ACCEPTED | `docs/development/common-changes/` | Configuration, port/adapter, and use-case playbooks. |

Phase 1 gate evidence is recorded in `docs/migration/phase-1-gate.md`; Phase 2 may begin.

## Phase 2

| ID | Status | Evidence | Note |
|---|---|---|---|
| A201 | ACCEPTED | `contracts/module/module.schema.json`, `contracts/module/amod.schema.json`, manifest fixtures | Closed manifest and deterministic bounded `.amod` package shape. |
| A202 | ACCEPTED | `contracts/module/control/v1/control.proto`, `protocol-compatibility.json`, `admin.openapi.yaml` | Generated control bindings, frozen RPC/field surface, authenticated administration edge. |
| A203 | ACCEPTED | `contracts/module/provider/v1/provider.proto`, sample protocol/tests | Identity, errors, deadlines, cancellation, idempotency, limits, and unsupported behavior. |
| A204 | ACCEPTED | `docs/decisions/0015-module-artifact-trust-and-execution.md`, trust/permission schemas | Manifest-bound Ed25519 signatures and exact fail-closed launcher receipts. |
| A205 | VERIFIED | lifecycle schema/document, `aseman-module-runtime` supervisor tests | Generation-fenced activate/drain/restore/rollback state machine. |
| A206 | VERIFIED | placement schema/document and reconciliation tests | Missing/unverified/unready placement and quorum routing rules. |
| A207 | VERIFIED | `tests/contracts/module`, `modules/sample-provider`, real-process lifecycle test | Reusable conformance kit and independently launched sample provider. |
| A208 | VERIFIED | bootstrap schemas and runtime signing/storage tests | Signed, expiring, permission-restricted recovery snapshot. |

Phase 2 gate evidence is recorded in `docs/migration/phase-2-gate.md`; Phase 3 may begin.

## Phase 3

| ID | Status | Evidence | Note |
|---|---|---|---|
| A301 | ACCEPTED | `contracts/capsule/encoding.md`, canonical and invalid vectors, `aseman-contracts::capsule` | Deterministic CBOR, domain-separated integrity, revision chain, tombstone, and relationship semantics. |
| A302 | ACCEPTED | `contracts/capsule/query/`, typed Rust query values and tests | Closed typed bounded queries, traversal projections, cursors, capabilities, and stable errors; raw provider query text is rejected. |
| A303 | ACCEPTED | `contracts/capsule/capabilities.schema.json`, registry, negotiation tests | Exact provider guarantees are negotiated without silent consistency weakening. |
| A304 | ACCEPTED | `contracts/capsule/kinds/`, generated capsule catalog | Complete initial core kind, logical field, relationship, index, retention, ownership, and native-table registry; ADR 0016 adds four subject-bound metadata document kinds and the column-free `document` field type. |
| A305 | ACCEPTED | `contracts/storage/postgres/core-mapping.json`, generated DDL/docs, `aseman-storage-postgres`, live PostgreSQL 16 conformance | Twenty native core tables, trusted guest binding catalog, typed repository, and versioned gRPC provider contract. |
| A306 | ACCEPTED | `contracts/capsule/guest/`, `aseman-storage-postgres::guest`, live PostgreSQL isolation test | Per-creature database/role lifecycle, typed multi-table schemas, revision catalog, bounded pools, and isolation specification; signed gateway implementation remains A401/A405/P4-04. |
| A307 | ACCEPTED | storage-class registry/semantics, generated PostgreSQL mapping and DDL, live append/uniqueness test | Thirteen native telemetry/audit/finance/outbox/realtime tables with explicit consistency, mutation, retention, and query policies; later P7/P8 services cannot weaken them. |
| A308 | ACCEPTED | `contracts/migration/legacy-transform-manifest.json` (0 blocked rows), `modules/storage-legacy`, ADRs 0016–0025, `docs/migration/work-units/P3-05.md` | Every A004 row, candidate template, object family, QuestDB table, Hashgraph family, and the OpenRaft store has a reviewed disposition; unknown records fail closed; runner evidence inputs are explicit. |
| A309 | ACCEPTED | `contracts/migration/protocol.md`, `docs/operations/storage-migration-runbook.md`, `aseman-domain::storage_migration`, `aseman-application::storage_migration`, `tests/migration` (live) | Export/import/verify/dual-write/delta/fenced cutover/rollback/retire protocol with semantic comparison; proven end to end on PostgreSQL 16. |
| A310 | VERIFIED | `tests/contracts/storage` | Reusable vector and behavioral harness; every provider must pass it before activation. |

## Phase 4

| ID | Status | Evidence | Note |
|---|---|---|---|
| A401 | ACCEPTED | `contracts/security/identity-v1.md`, `contracts/security/vectors/identity-v1.json`, `aseman-contracts::identity`, `aseman-domain::identity` | This artifact fixes the exact ADR 0009 formats and rules: versioned multicodec keys with multibase text, multihash key IDs, the domain-separated signed bytes, the signed-request proof, validation order, freshness, replay, epochs and rotation, revocation, trust roots, and legacy RSA verification. The vectors are generated from the code and checked byte for byte. The key directory, the replay store, and the verifier use case follow in P4-01. |
| A402 | ACCEPTED | `contracts/security/actions.json`, `contracts/security/policy-v1.md`, `docs/generated/security-action-registry.md`, `scripts/generate_security_registry.py` | 121 actions over 33 resource types. Every one of the 224 inventoried surfaces (A002 plus the module admin API) maps to exactly one action, checked in the gate. Rules replace the legacy guards with least privilege, and the LD-24 raw key access and custodial email login are `never`. |
| A403 | ACCEPTED | `contracts/security/policy-v1.md` (A403), `aseman-domain::capability`, `aseman-application::capability`, `CapsuleGrantStore` (live PostgreSQL conformance), grant fixtures in `tests/contracts/policy/decisions-v1.json` | The grant state machine covers issuance, delegation by intersection only, chain validity, descendant revocation, and explanation. A 2,000-round randomized property test shows no delegation amplifies authority. `core.capability_grant` is redefined for the full grant model (the retirement migration drops the writerless first shape). |
| A404 | ACCEPTED | `contracts/security/policy-v1.md`, `tests/contracts/policy/decisions-v1.json`, `aseman-domain::authority`, `aseman-policy-native` | The decision contract (request with caller-established facts, fixed evaluation order, reason codes, explanation, versions), with 24 normative hand-written cases. The reference provider reproduces them, and `PolicyDecisionPort` is typed on it. |
| A405 | ACCEPTED | `contracts/security/guest-v1.md`, `aseman-application::guest`, `aseman-capsule-repositories::workload`, `PostgresGuestKv`, live `live_guest_gateway` | Authenticated workload (A401 proof or node-registered VM handle), then server-side workload, program, creature, and active binding, with fail-closed chain checks. Policy authorizes with `same_creature`. The legacy KV operations run as sealed capsule revisions in the creature's role, and deletes and prefix listing are now correct. Live: two creatures' workloads write the same key and each reads only its own, a tampered catalog record cannot route, and a crossed program fails closed. |
| A406 | ACCEPTED | `contracts/security/runtime-matrix-v1.md`, `node/src/shell/authority.rs` | This is the per-runtime matrix. Every runtime's host calls reach one identified and authorized entry point. Host-mediated egress needs a destination grant (shadow on the legacy provider until grants exist), secrets and guest data are deny-by-default, and raw node keys are `never`. Direct network access by container and microVM workloads is owned by the P6 runtime providers. |

P3-01 evidence is recorded in `docs/migration/work-units/P3-01.md`; RL-005 and RL-006 remain open.
P3-02 evidence and rollback are recorded in `docs/migration/work-units/P3-02.md` and
`docs/operations/postgres-core-migration.md`.
P3-03 provider/isolation evidence and rollback are recorded in
`docs/migration/work-units/P3-03.md` and `docs/operations/postgres-guest-databases.md`;
RL-006 remains open and no guest gateway is activated before P4-04.
P3-04 storage-class evidence is recorded in `docs/migration/work-units/P3-04.md` and
`docs/operations/postgres-storage-classes.md`; RL-005, RL-008, and RL-011 remain open.

## Phase 5

| ID | Status | Evidence | Note |
|---|---|---|---|
| A501 | ACCEPTED | `contracts/vmm/openapi.json`, `aseman-contracts::vmm`, ADR 0029 | OpenAPI 3.1 contract with idempotency, resource versions, RFC 9457 problems, cursor pagination, SSE, terminal, capability negotiation, and the invocation, forwarding, file, build, snapshot, and proof operations the legacy runtimes need. Contract tests tie the Rust wire types to every schema. |
| A502 | ACCEPTED | `contracts/vmm/states.json`, `aseman-domain::vmm` | Desired and operation state machines; the table is tested against the domain functions. |
| A503 | ACCEPTED | `contracts/vmm/states.json`, `aseman-domain::vmm` | Node-owned desired generations with compare-and-set, command freshness (apply, replay, stale), forward-only observations, one-step reconciliation, and adoption of undesired instances (ADR 0022). |
| A504 | ACCEPTED | `contracts/vmm/backend/v1/backend.proto`, `modules/vmm-backend-grpc`, `tests/contracts/vmm-backend`, `modules/vmm-backend/native-legacy` | The backend contract and its conformance kit: describe, step, observe, run, forward, files, endpoints, usage, logs, verify, with request metadata and structured errors. The same suite passes directly and over gRPC, so the transport is transparent; the native backend passes it while running real programs. |
| A505 | ACCEPTED | `contracts/vmm/native-parity.json`, `docs/generated/vmm-native-parity.{json,md}`, `scripts/generate_vmm_parity.py` | Every A006 runtime operation and `IVmm` method has a destination, and the native capabilities are derived per runtime. 22 of the 27 runtime operations are verified by live tests (JavaScript and a real Docker daemon), 5 are open with recorded reasons (Modal credentials, an elpify proof fixture, a gateway-protocol container, two unused plan helpers), and all 46 node methods are `deleted` with P5-06. |


Phase 5 gate evidence is recorded in `docs/migration/phase-5-gate.md`; the node's VMM
cutover is ADR 0030 and the deletion evidence for RL-013 is in the removal ledger.

## Phase 6

| ID | Status | Evidence | Note |
|---|---|---|---|
| A601 | ACCEPTED | `contracts/vmm/nomad/mapping.json`, `modules/vmm-backend/nomad`, `src/job/tests.rs`, `tests/live_nomad.rs` | A workload is one Nomad job: the UUID is the job ID, the desired state is the group's count, and the desired generation travels in job meta and is what an observation reports. Resources, ingress ports, and services are mapped; egress policy, pause, snapshots, exec, files-in, and builds are refused rather than approximated, and `raw_exec`, privileged mode, host network, host PID, and host mounts are never set. The workload credential never enters the job spec. Verified live on Nomad v2.0.7 with the Docker driver: the allocation runs, HTTP reaches it in its bridge namespace, logs and usage come back, and a purge leaves nothing to observe. P6-04 made the declared capabilities derived from the A505 matrix rather than hand-written, and made deny-by-default egress enforced: a workload whose policy denies egress is refused on a network that cannot deliver it, selective allowances are refused rather than guessed, and the reference CNI network is proven to deny egress by a live probe that reaches the internet when run unrestricted. Runner images, QEMU, Firecracker, and volumes are P6-05 and P6-06. |
| A602 | ACCEPTED | `contracts/deploy/topology.json`, `docs/operations/topology.md`, `scripts/check_deploy_topology.py` | The compact, cluster, and host profiles with every service's ports, reachability, certificates, tokens, and privileges, and the bring-up and migration order. One node is one federation identity in every profile: workers and replicas come and go without changing it (ADR 0013). The contract is checked in `cargo xtask fast` against the code that decides it — the loopback-only A504 listener, the published guest path, and the absence of Docker or KVM access in the services that claim none. Compose and systemd files follow in Phase 9. |
| A607 | ACCEPTED | `aseman-domain::coordination`, `aseman-ports::coordination`, `aseman-ports::conformance::coordination`, `aseman-storage-postgres::coordination`, `aseman-application::singleton`, `modules/storage-postgres/tests/live_coordination.rs` | The coordination contract and its chaos cases: a transactionally locked lease row, database time, owner instance, expiry, and a strictly increasing fencing token per acquisition; renew and release compare owner and token; a destination-side guard refuses an effect from a fenced-out holder. Verified on PostgreSQL 16: eight replicas acquire simultaneously and exactly one is granted, and four replicas running the same worker loop are never inside the work at once. The VMM service's executor, observer, reconciler, and retention now run only under the lease. Two real defects were found and fixed by these tests — a release that deleted the row restarted the token counter, and `SELECT ... FOR UPDATE` on a missing row granted three replicas the same token. |
| A606 | ACCEPTED | `modules/vmm-backend/nomad/src/workers.rs`, `modules/vmm-backend/nomad/tests/live_workers.rs` | Worker cordon, drain, cancel, and return, as operator actions with an operator-scoped token the backend does not hold. Verified on Nomad v2.0.7: a cordon stops placement without disturbing running work, a drain moves work off, a cancelled drain leaves the worker cordoned until an operator says otherwise, and a workload whose container is killed out from under the scheduler is observed as not running and restarted as itself. A workload's ID and generation, and the Aseman node's identity, are unchanged throughout. Partition and clock-skew chaos fixtures are P6-06. |
| A603 | ACCEPTED | `contracts/vmm/agent/agent-v1.md`, `aseman-domain::agent`, `apps/aseman-vmm-agent`, `apps/aseman-vmm-agent/tests/live_firecracker.rs` | The agent protocol and privilege model: mutual TLS says which component calls, a signed short-lived grant says what it may do — one allocation, one administrator-declared profile, a set of operations, a deadline. No shell, no arbitrary path (an escaping name is refused, not normalized), no arbitrary device, no raw Firecracker API. Verified against Firecracker v1.17.0: a real process and API socket take a real machine configuration, and a boot that cannot succeed reports Firecracker's own reason instead of a running machine. This host has no `/dev/kvm`, so booting a guest is open and needs a KVM-capable host with an administrator-supplied kernel. |
| A604 | PARTIAL | `aseman-domain::agent::MachineState::allows`, `runtime_capabilities` in the Nomad backend, `docs/generated/vmm-native-parity.md` | Pause and resume semantics are fixed and enforced: pause is the runtime's pause through Firecracker's API, a pause of a machine that is not running is refused, and the API's pause may never degrade to stop and start. Per-runtime capabilities are derived from the A505 matrix and masked to what each backend can deliver, so an unsupported operation is reported rather than silently degraded. Snapshot semantics need a KVM-capable host and a portability tier (ADR 0011): P6-06. |
| A605 | ACCEPTED | `aseman-domain::volume`, `docs/operations/stateful-workload-moves.md` | The four portability tiers, and `plan_move`, which decides before anything is quiesced or copied whether a move is possible and what it would take, refusing by volume name when it is not. A `provider_local` volume may not leave its provider, a `portable_offline` volume crossing runtimes needs a declared snapshot format, and no workload moves between CPU architectures. The plan is the strongest requirement any one volume imposes. `recreation_is_lossless` exists so a caller asks before recreating a stateful workload as an empty one. The runbook fixes the offline-copy order and names starting the target as the irreversible point. Executing a move needs a provider that can snapshot: the Nomad backend refuses snapshots and the agent needs a KVM-capable host. |

## Phase 7

| ID | Status | Evidence | Note |
|---|---|---|---|
| A704 | ACCEPTED | `contracts/federation/directory/descriptors-v1.md`, `aseman-domain::federation` | Node and workload descriptor schemas with signing epochs, revocation, sequence and revision monotonicity, and expiry. A cached descriptor is replaced only by a strictly higher sequence or revision, so a replayed older descriptor cannot un-rotate a key or un-revoke an epoch, and a descriptor naming its own epoch as revoked is refused. The workload descriptor is minimal by design: where to send something and how to verify the answer, and nothing about the creature, program, capabilities, logs, presence, or data. Resolution grants no authority. |
| A705 | ACCEPTED | `contracts/federation/envelope-v1.md`, `aseman-domain::federation` | The envelope and everything the destination checks before authorization: version, that it is addressed to this node, distinct source and destination, expiry by the destination's own clock, a lifetime of at most 60 seconds, a hop limit of at most 4 that decreases and refuses at zero, an unseen bounded nonce, a real SHA-256 payload digest, and a named subject, target, and action. Thirteen cases, one per way a peer could be wrong. The transport and signature verification are P7-01's, over these rules. |
| A707 | ACCEPTED | `contracts/realtime/events-v1.md`, `aseman-domain::realtime`, `aseman-ports::realtime`, `aseman-ports::conformance::realtime`, `aseman-storage-postgres::realtime`, `modules/storage-postgres/tests/live_realtime.rs` | The event envelope, retention classes, dense per-stream ordering, monotonic checkpoints, replay bounds, delivery scope, and the transactional outbox. Verified on PostgreSQL 16: an event and its outbox row are written together, a gap and a repeat are both refused, a backwards checkpoint does not move the stored one, a claim is exclusive and only its holder may complete it, an expired claim returns to the queue without cleanup, and retention purges a transient event while keeping a durable one. Delivery is at least once and ordering is per stream, both stated rather than implied. |
| A701 | ACCEPTED | `contracts/public/openapi.json`, `docs/generated/public-api.md`, `scripts/generate_public_api.py` | The public HTTP contract, generated from the A402 action registry rather than written: 76 operations, one per registered signed shell action, and one surface withheld because its policy rule is `never`. Nothing is reachable over HTTP that the policy cannot authorize, and nothing authorizable is missing from the contract — the generator's `--check` runs in `cargo xtask fast` and was verified to fail on a tampered document. Fixes authentication (session or A401 proof), idempotency (required for every action that is not a read), RFC 9457 problems with a stable reason, and request tracing. Serving it — the hardened stack, its middleware, and the SSE and WebSocket streams — is the node shell's work against this contract. |
| A706 | PARTIAL | `aseman-domain::federation`, `aseman-storage-postgres::federation`, `contracts/federation/directory/descriptors-v1.md` | Trust rotation and revocation are enforced where they are decided: key epochs travel in the node descriptor, a descriptor naming its own epoch as revoked is refused, and sequences and revisions only move forward so a replayed descriptor cannot un-rotate a key or un-revoke an epoch. Bootstrap (how a node first learns a peer's key) and partition behavior runbooks need the federation transport, and are P7-01's. |
| A702 | OPEN | — | The canonical gateway RPC for network modules. The application path it would front is fixed: one session path, one action registry, one public contract (A701, P7-05). |

## Phase 8, 9, 10

| ID | Status | Evidence | Note |
|---|---|---|---|
| A801 | ACCEPTED | `contracts/metering/metering-v1.md`, `aseman-domain::finance` | Normalized dimensions, the cumulative-to-delta rule, and the settlement identity `(workload, interval start, provider sample)`. Two readings of different workloads or out of order are not an interval; a counter that went backwards is read as the new counter, never an unsigned underflow. Ingress is metered and never billable. Late samples settle on arrival because the identity does not depend on arrival order. |
| A802 | ACCEPTED | `contracts/finance/pricing-v1.md`, `aseman-domain::finance` | Integer minor units, a rate scale of one billion, rounding up, deterministic pricing with per-dimension lines, versioned lists that are never edited after publication, and refusal of both a rate that rounds to zero and a billable dimension the list forgot. |
| A803 | ACCEPTED | `contracts/finance/ledger-v1.md`, `aseman-domain::finance`, `aseman-storage-postgres::finance` | Append-only double entry; every record balances or is refused with no half-entry written; committing an idempotency key twice is success, not a second record. Holds set money aside and are a ceiling — capturing more than was held, or capturing a finished hold, is refused. A refund is a new balanced record carrying the original's price version, never an edit. |
| A804 | ACCEPTED | `contracts/finance/consensus-v1.md`, `aseman-domain::consensus`, `aseman-ports::consensus` | Monotonic finalized epochs that never reopen, a finalized order that only extends, a digest disagreement that is reported rather than repaired, and a provider switch allowed only at a finalized epoch whose checkpoint matches it with nothing in flight. Wrapping the embedded Hashgraph behind the port is RL-011. |
| A805 | ACCEPTED | `contracts/finance/enforcement-v1.md`, `aseman-domain::finance` | The ordered state machine none → notify → pause → stop, with grace and pause windows. Nothing is immediate, pause precedes stop so paying resumes rather than restarts, stop is not destroy, and every step acts through ordinary authorized VMM operations so it is auditable. |
| A806 | ACCEPTED | `docs/operations/finance-reconciliation.md`, `aseman-domain::finance` | Four discrepancy kinds, and a corrective entry proposed only for the one with a defensible arithmetic answer. Unsettled, unexplained, and unknown-price discrepancies are escalated to a person: a reconciler that silently moves money turns one bad charge into an unauditable series. Nothing is ever repaired automatically. |
| A807 | ACCEPTED | `tests/contracts/finance/golden-usage-to-journal.json`, `crates/aseman-domain/src/finance/golden.rs` | Six golden usage-to-price-to-journal cases run against the domain in the gate: the ordinary minute, rounding up on a fraction, a per-byte rate at the billion scale, a restarted workload's reset counter, an idle minute that invents no line, and unbillable ingress. Verified to fail when a fixture value is changed, so a billing change cannot pass unnoticed. |
| A007 (Phase 9) | PARTIAL | `aseman-domain::bootstrap`, `docs/migration/work-units/P9-01.md`, `docs/migration/phase-9-gate.md` | The bootstrap workflow's rules: idempotent, resumable, and rolling forward rather than undoing at and after the schema stage. Seven cases. `asemanctl`, images, one-command bootstrap, backup and restore, and moving tracked `dist/*` blobs out of source control are open and recorded. |
| Phase 10 | PARTIAL | `scripts/check_removal_ledger_due.py`, `docs/migration/acceptance.md`, `docs/migration/phase-10-gate.md` | The release gate that fails while an accepted phase's removal-ledger row records no outcome — it found eleven silent rows and each now records one. The acceptance assessment marks every criterion MET with evidence or OPEN with its obstacle. Fuzz, load, chaos, SBOM, signing, canary, and the rollback drills are open and each needs infrastructure outside this repository. |
| A703 | OPEN | — | Listener broker bind, handoff, drain, and failure semantics. Needed by the transport work that serves A701; the application path it fronts is already single (P7-05). |
| A708 | OPEN | — | Realtime provider topology and capacity model. The semantics and the durable provider are delivered (A707); topology and capacity numbers need a deployment to measure. |
| A901 | PARTIAL | `contracts/deploy/topology.json`, `docs/operations/topology.md` | The component, port, certificate, token, and privilege matrix is delivered and gate-checked (A602). Images and volumes are open with the packaging work. |
| A902 | PARTIAL | `aseman-domain::bootstrap`, `docs/migration/work-units/P9-01.md` | The bootstrap state machine and its resumable journal are delivered and tested. Upgrade, backup, and restore reuse the same shape and are not written. |
| A903 | PARTIAL | `docs/generated/current-cli-ops.{json,md}` | The legacy CLI is inventoried and gate-checked. The `asemanctl` command, output, and exit-code catalogue waits on RL-015. |
| A904 | PARTIAL | `contracts/deploy/topology.json` health rows, `aseman-vmm-http::health_router` | Health and readiness endpoints exist for the VMM and the node. Dependency semantics, readiness that distinguishes API service from singleton eligibility, and support-bundle redaction rules are open. |
| A905 | OPEN | — | Dashboards, alerts, SLOs, error budgets, and capacity assumptions. Needs a running deployment to measure. |
| A906 | OPEN | — | Release signing, SBOM, provenance, and vulnerability policy. Needs release infrastructure outside this repository; RL-018 stays open until it exists. |
| A1001 | PARTIAL | `xtask/src/main.rs` | `cargo xtask fast` and `full` are the executable matrix: architecture, generated-artifact freshness, contract checks, unit, conformance, and live integration suites. Platform and feature coverage beyond this host is open. |
| A1002 | PARTIAL | the live suites named in `docs/migration/status.md` | The failover and chaos scenarios the phase gates required are implemented and pass: worker loss, drain, replica loss, backend crash, database unreachability, envelope replay, and settlement retry. Load, soak, and broader chaos thresholds are open. |
| A1003 | OPEN | — | Canary decision criteria and abort thresholds. Needs a deployment. |
| A1004 | PARTIAL | `docs/migration/removal-ledger.md`, `scripts/check_removal_ledger_due.py` | The ledger is enforced: a release fails while an accepted phase's row records no outcome. It is not *closed* — the outstanding rows are listed with their blockers. |
| A1005 | PARTIAL | `docs/migration/acceptance.md` | Every acceptance criterion is marked MET with evidence or OPEN with its obstacle. A generated traceability report is open. |

