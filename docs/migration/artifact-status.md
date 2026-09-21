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

P3-01 evidence is recorded in `docs/migration/work-units/P3-01.md`; RL-005 and RL-006 remain open.
P3-02 evidence and rollback are recorded in `docs/migration/work-units/P3-02.md` and
`docs/operations/postgres-core-migration.md`.
P3-03 provider/isolation evidence and rollback are recorded in
`docs/migration/work-units/P3-03.md` and `docs/operations/postgres-guest-databases.md`;
RL-006 remains open and no guest gateway is activated before P4-04.
P3-04 storage-class evidence is recorded in `docs/migration/work-units/P3-04.md` and
`docs/operations/postgres-storage-classes.md`; RL-005, RL-008, and RL-011 remain open.
