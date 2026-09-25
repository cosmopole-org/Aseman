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
| RL-001 | MOVE | `apps/aseman-node/src/main.rs` composition/config/startup | node executable | `apps/aseman-node`, `aseman-config` | A003 plus startup fixtures (A008) | constructor-inject typed config; retain legacy env aliases for rollback | no business rules/direct env reads in `main` | Phase 1/10 |
| RL-002 | MOVE/MERGE | `apps/aseman-node/src/models/*` mixed domain/ports/DTOs | repository-wide node code | `aseman-domain`, `aseman-ports`, `aseman-contracts` | A005 plus type/state tests | move one type family at a time; compatibility re-exports | global model bucket has no authoritative type | Phase 1 |
| RL-003 | REWRITE | `apps/aseman-node/src/core/*` orchestration/service globals | actions, transports, VMM | `aseman-application` use cases | A005/A008 | strangler use cases with injected ports; route old call path on rollback | no service locator or driver imports in application/domain | Phase 1/10 |
| RL-004 | REWRITE | `apps/aseman-node/src/shell/api/actions/*` | TCP, WS, federation, guest shell execution | gateway adapters plus application use cases | A002/A005/A008 | translate one action family; retain fixture-compatible adapter | handlers contain translation only; superseded bodies removed | Phase 1/7 |
| RL-005 | REWRITE | RocksDB/QuestDB storage and physical keys | core, actions, VMM, finance, consensus | capsule ports, PostgreSQL, legacy migration provider | A004/A008 | export/import, dual write, compare, cutover; restore old binding | node/application import no concrete DB type | Phase 3 |
| RL-006 | REWRITE | legacy VM database host calls | embedded VM runtimes | signed guest-data proxy and creature database roles | A004/A005/A008; ADR 0001 | provision per-creature database/role and switch binding; restore old generation | no workload-selected namespace/raw storage handle; old calls removed | Phase 3/4/5 |
| RL-007 | MOVE | `apps/aseman-node/src/drivers/security.rs` signing | node actions/federation | identity/crypto adapter plus policy provider | A005/A010 | introduce typed ports; retain verified legacy adapter | all authorization uses one policy decision path | Phase 4 |
| RL-008 | REWRITE | `apps/aseman-node/src/drivers/signaler.rs` process-local realtime | actions/VMM/network | realtime port and durable/in-memory providers | A005/A008 | transactional outbox and offset migration; route back to legacy during window | no authoritative process-local production state | Phase 7 |
| RL-009 | MERGE/DEPRECATE | TCP/WS client sessions | external clients | transport-neutral gateway plus HTTP/default and legacy framing adapters | A002/A005/A008 | shared dispatch first; drain/rollback listener routing | legacy adapters contain framing only, then expire | Phase 7/10 |
| RL-010 | REWRITE | custom federation transport/routing | remote Caspar nodes | federation HTTP provider and signed-envelope use cases | A002/A005/A008/A010 | dual-protocol compatibility; destination-authorized rollback | legacy federation protocol expires after parity window | Phase 7/10 |
| RL-011 | MOVE | embedded Hashgraph implementation | finance/actions | `modules/consensus/hashgraph` | finance fixtures A008/A807 | adapter + financial epoch checkpoint; restore prior epoch provider | node finance imports only consensus port | Phase 8 |
| RL-012 | REWRITE/DELETE | OpenRaft cluster mesh | cluster CLI/node startup | coordination provider and Nomad topology, or optional role per ADR | A007/A008/A014 | role-specific migration; retain old topology only until ADR gate | no overlapping scheduler/leadership authority | Phase 6/10 |
| RL-013 | REWRITE | embedded VMM and runtime globals | node actions/host calls | `aseman-vmm`, native backend, worker agent, guest gateway | A002/A005/A006/A008 | operation-by-operation HTTP extraction; endpoint rollback | node links no runtime engines/globals | Phase 5 |
| RL-014 | REWRITE/DEPRECATE | compile-time `caspar-vm-plugins` aggregation | `caspar-node`, `casparctl vms` | signed runtime/provider modules | A001/A006/A007/A008 | preserve native provider until module parity; rollback routing generation | runtime replacement requires no node rebuild | Phase 2/5/10 |
| RL-015 | MOVE/DEPRECATE | `cmd/casparctl`, `client-cli`, `sdk` | operators/users | `apps/asemanctl`, generated clients/SDKs | A007/A008 | Aseman commands plus Caspar shims | aliases isolated, warned, and removed at ADR date | Phase 1/9/10 |
| RL-016 | REWRITE | root scripts and combined node image | operators/CI | `xtask`, deploy profiles, separate images, bootstrap, `asemanctl` lifecycle commands | A007/A008/A009 | stage idempotent workflows; moved/removed scripts stay in Git history as rollback path | giant installer/multi-process assumptions removed | Phase 9 |
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
| RL-001 | **Canonical implementation moved; alias retained in place.** `apps/aseman-node` owns the full implementation, composition, key generator, diagnostics, and ADR-0004 alias binaries. The former `node/` proxy package is deleted. Alias expiry waits on the compatibility window and RL-004's handler migration, not another binary move. |
| RL-002 | **Partly moved.** The domain and port families this migration touched — workloads, identity, capability, guest, VMM, coordination, federation, realtime, finance, volume, bootstrap — live in `aseman-domain` and `aseman-ports`. `apps/aseman-node/src/models/*` still holds the legacy DTOs the legacy transports frame. Open until those transports retire (RL-009). |
| RL-003 | **Partly rewritten.** The use cases this migration needed are in `aseman-application`. `apps/aseman-node/src/core/*` keeps the legacy orchestration the legacy actions still call. Open with RL-004. |
| RL-004 | **Replaced at the contract, transport, composition, and storage edges, not at the code.** Every action is registered (A402), published through the generated public contract (A701), admitted by the hardened `aseman-public-http` edge at `modules/network/http`, served by the composed `aseman-public-service` (A401/A402/execution/durable idempotency behind `ServePublicAction`), and mutations are durably idempotent over PostgreSQL (migration 0010). The handlers still live in `apps/aseman-node/src/shell/api/actions/*`; the node must supply the `ActionExecutor` port over migrated use cases and migrate those bodies, which remains open. |
| RL-007 | **Replaced.** Signing and verification are `aseman-identity-native` behind `IdentityVerifier` and `KeyDirectory`, proven by live identity tests (P4-01). `apps/aseman-node/src/drivers/security.rs` remains as the legacy adapter inside its ADR-0004 window. |
| RL-008 | **Replaced.** The durable realtime provider, its outbox, and its checkpoints are delivered and proven live (P7-04, A707). `apps/aseman-node/src/drivers/signaler.rs` remains until subscriptions are served over the new transport. |
| RL-009 | **Reduced to framing.** The legacy TCP and WebSocket sessions share one application path, and `scripts/check_legacy_transports.py` fails a release if either grows logic of its own (P7-05). The replacement HTTP edge and its composed service exist (P7-06); deletion waits on node composition supplying the ports, HTTP becoming the advertised default, and ADR 0004's window. |
| RL-010 | **Replacement provider delivered; composition and deletion gate remain.** `aseman-federation-http` owns the actual PostgreSQL state plus mandatory-mTLS inbound and outbound HTTP, A401/A705 verification, destination authorization, signed responses, retries, and circuit breaking. The legacy protocol stays until the node composes the signer/verifier/executor, deployed-cluster parity is observed, and ADR 0004's window expires. |
| RL-011 | **Implementation and adapter moved; composition open.** `modules/consensus/hashgraph` owns the Babble engine, peer transport, RocksDB event store, and all 213 original tests. `HashgraphConsensusProvider` implements the port over the engine's real application proxy: submissions enter Babble, committed blocks finalize records, pending work blocks adoption, and snapshots/checkpoints preserve the epoch state. The node keeps only its legacy `IChain` finance/action translation. Deletion waits on composing the financial provider, a live peer checkpoint switch, and the rollback observation. |
| RL-015 | **Canonical ownership consolidated, command set incomplete.** `apps/asemanctl` owns both the implementation and its warning alias binary; `apps/aseman-client` owns the compatibility client, and creature-implementation guidance lives in `docs/development/creature-implementation.md`. The proxy roots are deleted. Required administration groups, generated public clients/SDKs, stable structured output, and compatibility-expiry observation remain open. |
| RL-017 | **Archived and integrated.** The duplicate `wiki/` root is deleted, historical Caspar pages are status-labelled under `docs/legacy/caspar`, and `docs/README.md` is the sole current portal. Link and generated-inventory checks guard the result. |
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
  handoff planner (`modules/storage/rocksdb-legacy/src/vmhandoff.rs`); every instance is adopted
  into `core.workload` or explicitly stopped, with the operator's decisions bound to the
  export digest (P5-05, `docs/operations/vmm-handoff-runbook.md`). `ModalVolume` handles
  remain unadopted and stay the P6 runtime module's obligation.

RL-014 stays open only for dynamic replacement. The aggregation crate now lives at
`modules/vmm-backend/native-legacy/crates/caspar-vm-plugins`, beside the backend, and
all seven implementations live under `modules/runtime`; the node links neither. The
native backend still compiles its selected runtimes in, so loading signed runtime
modules without a backend rebuild remains Phase 10's gate.

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
