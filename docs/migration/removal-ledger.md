---
status: ACCEPTED
owner: migration/P0-06
source_of_truth: current repository plus plan/migration/15-agent-execution-guide.md
last_verified_commit: 800df24076c7
verification: python3 scripts/generate_removal_ledger_children.py --check
---

# Removal ledger

This is the parent subsystem ledger required by A011. The exhaustive generated child
ledger is `docs/generated/removal-ledger-children.json`, summarized in the adjacent
Markdown file. It expands A003 configuration keys, A004 storage families, A002/A005
protocol and call surfaces, runtimes, commands/scripts, packages, artifacts, and
compatibility aliases. No row authorizes deletion without its characterization and
replacement gates.

Target release values are migration phases. Caspar compatibility expiry follows ADR
0004: two Aseman minor releases and at least 180 days after the first stable replacement.

| ID | Disposition | Current owner/path | Current callers | Target owner | Characterization evidence | Migration / rollback | Removal condition | Target |
|---|---|---|---|---|---|---|---|---|
| RL-001 | MOVE | `node/src/main.rs` composition/config/startup | node executable | `apps/aseman-node`, `aseman-config` | A003 plus startup fixtures (A008) | constructor-inject typed config; retain legacy env aliases for rollback | no business rules/direct env reads in `main` | Phase 1/10 |
| RL-002 | MOVE/MERGE | `node/src/models/*` mixed domain/ports/DTOs | repository-wide node code | `aseman-domain`, `aseman-ports`, `aseman-contracts` | A005 plus type/state tests | move one type family at a time; compatibility re-exports | global model bucket has no authoritative type | Phase 1 |
| RL-003 | REWRITE | `node/src/core/*` orchestration/service globals | actions, transports, VMM | `aseman-application` use cases | A005/A008 | strangler use cases with injected ports; route old call path on rollback | no service locator or driver imports in application/domain | Phase 1/10 |
| RL-004 | REWRITE | `node/src/shell/api/actions/*` | TCP, WS, federation, guest shell execution | gateway adapters plus application use cases | A002/A005/A008 | translate one action family; retain fixture-compatible adapter | handlers contain translation only; superseded bodies removed | Phase 1/7 |
| RL-005 | REWRITE | RocksDB/QuestDB storage and physical keys | core, actions, VMM, finance, consensus | capsule ports, PostgreSQL, legacy migration provider | A004/A008 | export/import, dual write, compare, cutover; restore old binding | node/application import no concrete DB type | Phase 3 |
| RL-006 | REWRITE | legacy VM database host calls | embedded VM runtimes | signed guest-data proxy and creature database roles | A004/A005/A008; ADR 0001 | provision per-creature database/role and switch binding; restore old generation | no workload-selected namespace/raw storage handle; old calls removed | Phase 3/4/5 |
| RL-007 | MOVE | `node/src/drivers/security.rs` signing | node actions/federation | identity/crypto adapter plus policy provider | A005/A010 | introduce typed ports; retain verified legacy adapter | all authorization uses one policy decision path | Phase 4 |
| RL-008 | REWRITE | `node/src/drivers/signaler.rs` process-local realtime | actions/VMM/network | realtime port and durable/in-memory providers | A005/A008 | transactional outbox and offset migration; route back to legacy during window | no authoritative process-local production state | Phase 7 |
| RL-009 | MERGE/DEPRECATE | TCP/WS client sessions | external clients | transport-neutral gateway plus HTTP/default and legacy framing adapters | A002/A005/A008 | shared dispatch first; drain/rollback listener routing | legacy adapters contain framing only, then expire | Phase 7/10 |
| RL-010 | REWRITE | custom federation transport/routing | remote Caspar nodes | federation HTTP provider and signed-envelope use cases | A002/A005/A008/A010 | dual-protocol compatibility; destination-authorized rollback | legacy federation protocol expires after parity window | Phase 7/10 |
| RL-011 | MOVE | embedded Hashgraph implementation | finance/actions | `modules/consensus/hashgraph` | finance fixtures A008/A807 | adapter + financial epoch checkpoint; restore prior epoch provider | node finance imports only consensus port | Phase 8 |
| RL-012 | REWRITE/DELETE | OpenRaft cluster mesh | cluster CLI/node startup | coordination provider and Nomad topology, or optional role per ADR | A007/A008/A014 | role-specific migration; retain old topology only until ADR gate | no overlapping scheduler/leadership authority | Phase 6/10 |
| RL-013 | REWRITE | embedded VMM and runtime globals | node actions/host calls | `aseman-vmm`, native backend, worker agent, guest gateway | A002/A005/A006/A008 | operation-by-operation HTTP extraction; endpoint rollback | node links no runtime engines/globals | Phase 5 |
| RL-014 | REWRITE/DEPRECATE | compile-time `caspar-vm-plugins` aggregation | `caspar-node`, `casparctl vms` | signed runtime/provider modules | A001/A006/A007/A008 | preserve native provider until module parity; rollback routing generation | runtime replacement requires no node rebuild | Phase 2/5/10 |
| RL-015 | MOVE/DEPRECATE | `cmd/casparctl`, `client-cli`, `sdk` | operators/users | `apps/asemanctl`, generated clients/SDKs | A007/A008 | Aseman commands plus Caspar shims | aliases isolated, warned, and removed at ADR date | Phase 1/9/10 |
| RL-016 | REWRITE | root scripts and combined node image | operators/CI | `xtask`, deploy profiles, separate images, bootstrap | A007/A008/A009 | stage idempotent workflows; scripts remain rollback path temporarily | giant installer/multi-process assumptions removed | Phase 9 |
| RL-017 | ARCHIVE/GENERATE | root README and `wiki/*` duplicated current truth | users/agents | status-labelled `docs/` plus generated references | documentation link/inventory checks | archive legacy Caspar docs; restore from Git if needed | no contradictory authoritative inventory | Phase 9/10 |
| RL-018 | DELETE | tracked `dist/*` binaries/runtime blobs | build/install scripts | signed release/OCI artifacts | A001/A007 plus artifact parity | publish and verify external artifacts before removal | builds/install no longer consume tracked blobs | Phase 9 |
| RL-019 | DELETE | `/creatures/login` custodial RSA keys (`link::UserPrivateKey::*`) returned on email login | legacy login clients | ADR 0009 user-held Ed25519 keys with proof-of-possession enrollment (P4-01) | A308 custody verification (ADR 0019) | keys stay only in the immutable legacy source; never exported to Aseman | canonical enrollment or accepted verification-only status for every affected identity; login no longer returns key material | Phase 4/10 |

## Phase 2 evidence for RL-014

The signed out-of-process module replacement path now installs, validates, stages,
activates, drains, and rolls back the sample provider without rebuilding the node.
RL-014 remains open: no embedded runtime or `caspar-vm-plugins` path is deletable until
the Phase 5 runtime/VMM parity gates and the Phase 10 compatibility window pass.

## Outstanding rows within an accepted phase's window

`scripts/check_removal_ledger_due.py` fails a release while a row whose phase gate has
been accepted records no outcome. These are the outcomes. Where a row is still open, it
says so and says what is in the way — silence is what the check refuses.

| Row | Outcome |
|---|---|
| RL-001 | **Replaced, not deleted.** `aseman-config` owns typed configuration and every clean crate reads it; `node/src/main.rs` still composes the legacy node. The deletion gate waits on the `apps/aseman-node` executable (RL-015's sibling), which is Phase 9 packaging work. |
| RL-002 | **Partly moved.** The domain and port families this migration touched — workloads, identity, capability, guest, VMM, coordination, federation, realtime, finance, volume, bootstrap — live in `aseman-domain` and `aseman-ports`. `node/src/models/*` still holds the legacy DTOs the legacy transports frame. Open until those transports retire (RL-009). |
| RL-003 | **Partly rewritten.** The use cases this migration needed are in `aseman-application`. `node/src/core/*` keeps the legacy orchestration the legacy actions still call. Open with RL-004. |
| RL-004 | **Replaced at the contract, not at the code.** Every action is registered (A402) and published through the generated public contract (A701); the handlers still live in `node/src/shell/api/actions/*`. Serving them over the new stack is the open transport work. |
| RL-007 | **Replaced.** Signing and verification are `aseman-identity-native` behind `IdentityVerifier` and `KeyDirectory`, proven by live identity tests (P4-01). `node/src/drivers/security.rs` remains as the legacy adapter inside its ADR-0004 window. |
| RL-008 | **Replaced.** The durable realtime provider, its outbox, and its checkpoints are delivered and proven live (P7-04, A707). `node/src/drivers/signaler.rs` remains until subscriptions are served over the new transport. |
| RL-009 | **Reduced to framing.** The legacy TCP and WebSocket sessions share one application path, and `scripts/check_legacy_transports.py` fails a release if either grows logic of its own (P7-05). Deletion waits on the HTTP transport and ADR 0004's window. |
| RL-010 | **Replaced by contract.** The federation envelope, descriptors, replay protection, and destination authorization are delivered and proven between two live clusters (P7-02, P7-03, Phase 7 gate). The legacy transport stays until the federation HTTP provider ships. |
| RL-011 | **Designed, not yet moved.** Finance depends on no consensus type: the ledger, pricing, and metering ports are independent (Phase 8 gate). `aseman-ports::consensus::ConsensusProvider` and the epoch rules in `aseman-domain::consensus` now define what an adapter must do — a provider changes only at a finalized epoch, with a checkpoint that matches it and nothing in flight. Wrapping the embedded Hashgraph in that port is the remaining work. |
| RL-015 | **Open.** `cmd/casparctl` is still the CLI. The bootstrap workflow it will drive is delivered as rules (Phase 9), and `asemanctl` with its Caspar shims is the packaging work. |
| RL-019 | **Closed at the edge.** `/creatures/login` is refused by policy (`never`) and is the one surface deliberately withheld from the public contract (A701). The custodial keys remain only in the immutable legacy source and are never exported (ADR 0019, ADR 0023). |

## Phase 5 evidence for RL-013

RL-013 passes both gates with Phase 5 (`docs/migration/phase-5-gate.md`):

- **Replacement gate.** `aseman-vmm` (A501/A502/A503) and the native backend (A504)
  serve every VM operation the node used to perform in process. Parity is generated,
  not asserted: `docs/generated/vmm-native-parity.md` verifies 22 of the 27 runtime
  operations against the real engines and marks all 46 node methods `deleted`. The port
  conformance kit runs against the backend in-process, over gRPC, and over HTTP.
- **Deletion gate.** The node links no runtime engine and holds no second lifecycle
  path: `IVmm`, the bridge and its packet router, the SDK host bridge, the global
  host-call callback, bootstrap, and the VM gateway service are deleted (P5-06). ADR
  0030 records the cutover.
- **ADR 0022 obligation.** The legacy observed link families are read only by the
  handoff planner (`modules/storage-legacy/src/vmhandoff.rs`); every instance is adopted
  into `core.workload` or explicitly stopped, with the operator's decisions bound to the
  export digest (P5-05, `docs/operations/vmm-handoff-runbook.md`). `ModalVolume` handles
  remain unadopted and stay the P6 runtime module's obligation.

RL-014 stays open. `node/crates/caspar-vm-plugins` is now the *backend's* aggregation
crate, not the node's — no node rebuild is needed to change a runtime's wiring — but the
crate still lives under `node/` and the runtimes are still compiled in. Moving it under
`modules/vmm-backend/` and loading signed runtime modules is Phase 10's.

## ADR 0022 obligations (RL-005, RL-013)

- The observed VM runtime link families (`VmInstance`, `VmStatus`, `VmStartedAt`,
  `VmOwnerProgram`, `vmDistributed`, container, terminal, build, proxy-correlation, and
  `Modal*` handles) are the native-legacy VMM backend's runtime state. They are not
  exported as Aseman capsules. RL-005 cutover and deletion must not remove them from the
  legacy RocksDB before RL-013 passes its replacement and deletion gates.
- P5 reconciliation must adopt every instance in the export's VMM handoff inventory into
  `core.workload`, or explicitly stop it. `ModalVolume` handles must be adopted by the P6
  runtime module or explicitly released by an operator; they must never be orphaned.
- `vmDistribution` is deleted with RL-012 (ADR 0012); placement moves to the P6 scheduler.

## ADR 0001 obligations

- Any shared `guest_capsules` implementation introduced before Phase 3 is non-authoritative
  and must be deleted rather than migrated into the target architecture.
- Legacy VM database operations remain only as characterized compatibility input until the
  signed proxy and per-creature database/role path passes replacement and rollback tests.
- Provider role mappings, database names, pool state, and credentials must never become
  caller-selectable compatibility fields.
