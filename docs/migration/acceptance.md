---
status: CURRENT
owner: migration
source_of_truth: plan/migration/10-verification-and-acceptance.md
last_verified_commit: eebb9c5
verification: cargo xtask fast; the live suites named per row
---

# Acceptance assessment

Written for: whoever picks this migration up next.

Each criterion from `plan/migration/10-verification-and-acceptance.md` is marked **MET**
with its evidence, or **OPEN** with what is in the way. Nothing is marked met on the
strength of an intention.

## Architecture

| Criterion | State | Evidence |
|---|---|---|
| Domain and application import no concrete adapters | MET | `cargo xtask arch`, in every gate run |
| The node contains no runtime implementation | MET | Phase 5 gate; `node/Cargo.toml` links no runtime crate |
| No dynamic library ABI for providers | MET | Providers are processes behind gRPC (A504) and HTTP (A501) |
| Every provider publishes a manifest and passes its suite | MET | A504 kit passes for native and Nomad backends |
| Module activation is health-checked, drainable, rollback-safe | MET | Phase 2, `aseman-sample-provider` |
| One authoritative implementation per capability | PARTIAL | True for every capability this migration replaced; the legacy node still holds the paths listed in the removal ledger's outstanding rows |
| Dependency cycles and forbidden imports fail CI | MET | `check_architecture` |
| No phase crosses its gate with a MISSING artifact | MET | `docs/migration/artifact-status.md`; Phase 9 is recorded PARTIAL rather than crossed |

## Capsule storage

| Criterion | State | Evidence |
|---|---|---|
| Core, guest, telemetry, audit, outbox, realtime data use capsule contracts | MET | Phase 3; realtime and outbox added in P7-04 |
| Separate table per core entity; no universal KV | MET | `docs/generated/postgres-core-mapping.md`; a test asserts the core migration contains no `jsonb` |
| A database and role per creature | MET | `live_guest_gateway` |
| VMs of one creature share its database; of different creatures cannot reach each other's | MET | `live_guest_gateway` isolation cases |
| VMs cannot nominate the creature, database, or role | MET | A405: resolution is server-side from the authenticated workload |
| Signed guest access rejects expired, replayed, revoked, wrong-audience, wrong-epoch, substituted requests | MET | A401 suites |
| Capsule revisions prevent lost updates | MET | Storage conformance |
| Migration preserves kind, schema, ownership, revisions, tombstones, hashes | MET | Phase 3 export/import determinism tests |
| Interrupted migration resumes; cutover rolls back in the window | MET | `bounded_export_import_is_deterministic_resumable_and_idempotent` |
| Audit data append-only and verifiable after migration | MET | Phase 3 |

## VMM and cluster

| Criterion | State | Evidence |
|---|---|---|
| Native and Nomad providers pass identical semantics | MET | `check_backend` against both; Phase 6 gate |
| Providers switch without rebuilding the node | MET | The backend is a separate process behind A504 |
| Lifecycle, logs, exec, events, usage over the HTTP contract | MET | A501; P5-07 moved logs onto it |
| Idempotent retries do not duplicate workloads | MET | A501 idempotency store; the Nomad job ID *is* the workload UUID |
| Compact mode on one host | MET | A602, and the live single-node cluster these tests ran against |
| Production mode with three/five servers and expandable workers | PARTIAL | The topology is contracted and worker add/drain is proven; a three-server cluster has not been stood up here |
| Adding or removing a worker does not change node identity | MET | P6-03, asserted live |
| Losing a replica changes no identity and duplicates no fenced effect | MET | P6-03A, proven live on PostgreSQL |
| Firecracker only on eligible, authorized workers | MET | A603: capability refused before any work; grants name one allocation |
| `raw_exec` absent from production defaults | MET | A601 forbids it; a mapping test asserts it |
| Restarts reconcile desired and observed state | MET | Phase 5 system test kills the backend and converges |

## Security

| Criterion | State | Evidence |
|---|---|---|
| Every sensitive action reaches the decision point | MET | A402: every inventoried surface maps to an action, checked in the gate |
| A child cannot amplify a parent's rights | MET | A403 property test, 2,000 rounds |
| Revocation reaches sessions and descendants | MET | A403 |
| Workloads hold no database credentials | MET | ADR 0001; A602 check scans for it |
| Nomad identity never bypasses Aseman policy | MET | A601; the workload still signs its guest calls |
| Federation requests signed, expiry-checked, replay-protected | MET | A705, `live_federation`, `live_two_clusters` |
| Destinations independently authorize cross-node actions | MET | Phase 7 gate: no facts are established by an envelope arriving |
| Policy decisions and administrative mutations produce audit capsules | MET | A404 decision audit |

## Network, federation, realtime

| Criterion | State | Evidence |
|---|---|---|
| HTTP is the shipped default | PARTIAL | The contract is generated and authoritative (A701); serving it is open |
| Adapters pass the same application contract tests | MET | `check_legacy_transports.py`: they contain framing only |
| Any workload resolves any target's minimal descriptor | MET | A704 |
| Discovery grants no operational rights | MET | Phase 7 gate |
| Authorized operations work across nodes | MET | `live_two_clusters` |
| Realtime survives restart and deduplicates | MET | A707: the log is authoritative and durable; consumers deduplicate on event ID |
| Partitions recover without duplicate execution or loops | MET | Request-ID deduplication and the 4-hop limit, both tested |

## Finance and metering

| Criterion | State | Evidence |
|---|---|---|
| Charges use measured usage | MET | Phase 8: cumulative counters become interval deltas |
| Every interval settles at most once; backfill after outage | MET | `live_finance`: triple-committed settlements, and a late interval |
| Every ledger mutation balanced and idempotent | MET | `live_finance`; an unbalanced record leaves no half-entry |
| Every charge traces to workload, interval, sample, price version | MET | The idempotency key *is* the settlement identity |
| Insufficient funds uses policy and authorized operations | MET as rules | `enforcement`; the enforcement loop is open |
| Consensus providers change only at a verified epoch | OPEN | `ConsensusProvider` is not yet extracted (RL-011), though finance no longer depends on any consensus type |

## Packaging and operations

| Criterion | State | Evidence |
|---|---|---|
| Node, VMM, meter have separate least-privilege artifacts | PARTIAL | Node, VMM, two backends, and the agent are separate binaries with separate privileges (A602); the meter is not a binary yet |
| Compact setup through one idempotent command | OPEN | Phase 9 gate: the rules exist, the command does not |
| Re-running bootstrap is safe | MET as rules | P9-01: re-running a finished stage is an error |
| Failed stages resume or roll back without destroying data | MET as rules | P9-01: roll-forward at and after the schema stage |
| Backup and clean restore tested | OPEN | Phase 9 |
| Formatting, Clippy, tests, compatibility gates in CI | MET | `cargo xtask fast` |
| Releases commit no generated binaries | OPEN | RL-018: `dist/*` stays until a signed-artifact pipeline replaces it |

## Documentation and comprehension

| Criterion | State | Evidence |
|---|---|---|
| Scoped `AGENTS.md` with targeted checks | MET | Root `AGENTS.md` |
| Current, target, legacy, decision behavior distinguished | MET | `docs/decisions/`, `plan/migration/`, `docs/generated/` |
| Machine-readable routes, kinds, contracts, config, topology | MET | `contracts/` and `docs/generated/`, all gate-checked |
| CLI, API, schema, config references generated from source | MET | `generate_public_api.py`, `generate_current_surface_inventories.py`, and the rest |
| Generated freshness passes CI | MET | Every generator runs `--check` in the gate |
| The removal ledger has no overdue item | MET | `check_removal_ledger_due.py`, which found eleven silent rows and now fails on any new one |

## Clean code and structure

| Criterion | State | Evidence |
|---|---|---|
| One workspace, lockfile, toolchain, lint policy, task runner | MET | Phase 1 |
| Explicit `apps`/`crates`/`modules`/`contracts`/`docs`/`tests`/`xtask` ownership | MET | The tree |
| No broad lint suppressions | MET | Quality baseline ratchet |
| Direct environment reads outside the config adapter rejected | MET | The ratchet; all four reads are in `aseman-config` |
| Internal APIs avoid unvalidated JSON and stringly-typed states | PARTIAL | True of every new contract; the legacy action handlers still pass `serde_json::Value` |
| Shared session behavior has one transport-neutral owner | MET | `check_legacy_transports.py` |
| Bounded batches and declared indexes on billing, outbox, guest paths | MET | Every store query takes a limit; the migrations declare their indexes |
| Hot paths have ratcheted budgets | OPEN | Phase 10 benchmarks |

## Required suites

Unit, architecture, capsule conformance, VMM conformance, policy property, contract, and
integration suites all run. **Fuzz, load, and full chaos suites are open**; the chaos
cases that the phase gates required — worker loss, drain, replica loss, backend crash,
database unreachability — are implemented and pass.

## Honest summary

The architecture is in place and proven against real infrastructure: PostgreSQL, Docker,
Nomad, and Firecracker. What remains is mostly **delivery**, not design — serving the
public contract over HTTP, packaging binaries and images, a one-command bootstrap, the
consensus adapter, and the load and fuzz suites. Each is named in a gate document or the
removal ledger, and `check_removal_ledger_due.py` now fails a release that lets one of
them slip silently.
