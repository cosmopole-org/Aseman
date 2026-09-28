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
| RL-004 | REWRITE | `apps/aseman-node/src/api/actions/*` | TCP, WS, federation, guest shell execution | gateway adapters plus application use cases | A002/A005/A008 | translate one action family; retain fixture-compatible adapter | handlers contain translation only; superseded bodies removed | Phase 1/7 |
| RL-005 | REWRITE | RocksDB/QuestDB storage and physical keys | core, actions, VMM, finance, consensus | capsule ports, PostgreSQL, legacy migration provider | A004/A008 | export/import, dual write, compare, cutover; restore old binding | node/application import no concrete DB type | Phase 3 |
| RL-006 | REWRITE | legacy VM database host calls | embedded VM runtimes | signed guest-data proxy and creature database roles | A004/A005/A008; ADR 0001 | provision per-creature database/role and switch binding; restore old generation | no workload-selected namespace/raw storage handle; old calls removed | Phase 3/4/5 |
| RL-007 | MOVE | `apps/aseman-node/src/adapters/security.rs` signing | node actions/federation | identity/crypto adapter plus policy provider | A005/A010 | introduce typed ports; retain verified legacy adapter | all authorization uses one policy decision path | Phase 4 |
| RL-008 | REWRITE | `apps/aseman-node/src/adapters/signaler.rs` process-local realtime | actions/VMM/network | realtime port and durable/in-memory providers | A005/A008 | transactional outbox and offset migration; route back to legacy during window | no authoritative process-local production state | Phase 7 |
| RL-009 | MERGE/DEPRECATE | TCP/WS client sessions | external clients | transport-neutral gateway plus HTTP/default and legacy framing adapters | A002/A005/A008 | shared dispatch first; drain/rollback listener routing | legacy adapters contain framing only, then expire | Phase 7/10 |
| RL-010 | REWRITE | custom federation transport/routing | remote Caspar nodes | federation HTTP provider and signed-envelope use cases | A002/A005/A008/A010 | dual-protocol compatibility; destination-authorized rollback | legacy federation protocol expires after parity window | Phase 7/10 |
| RL-011 | MOVE | embedded Hashgraph implementation | finance/actions | `modules/consensus/hashgraph` | finance fixtures A008/A807 | adapter + financial epoch checkpoint; restore prior epoch provider | node finance imports only consensus port | Phase 8 |
| RL-012 | REWRITE/DELETE | OpenRaft cluster mesh | cluster CLI/node startup | coordination provider and Nomad topology, or optional role per ADR | A007/A008/A014 | role-specific migration; retain old topology only until ADR gate | no overlapping scheduler/leadership authority | Phase 6/10 |
| RL-013 | REWRITE | embedded VMM and runtime globals | node actions/host calls | `aseman-vmm`, native backend, worker agent, guest gateway | A002/A005/A006/A008 | operation-by-operation HTTP extraction; endpoint rollback | node links no runtime engines/globals | Phase 5 |
| RL-014 | REWRITE/DEPRECATE | compile-time `caspar-vm-plugins` aggregation | `caspar-node`, `casparctl vms` | signed runtime/provider modules | A001/A006/A007/A008 | preserve native provider until module parity; rollback routing generation | runtime replacement requires no node rebuild | Phase 2/5/10 |
| RL-015 | MOVE/DEPRECATE | `cmd/casparctl`, `client-cli`, `sdk` | operators/users | `apps/asemanctl`, generated clients/SDKs | A007/A008 | Aseman commands plus Caspar shims | aliases isolated, warned, and removed at ADR date | Phase 1/9/10 |
| RL-016 | REWRITE | root scripts and combined node image | operators/CI | `xtask`, deploy profiles, separate images, bootstrap, `asemanctl` lifecycle commands | A007/A008/A009 | stage idempotent workflows; moved/removed scripts stay in Git history as rollback path | giant installer/multi-process assumptions removed | Phase 9 |
| RL-017 | ARCHIVE/GENERATE | root README and `wiki/*` duplicated current truth | users/agents | status-labelled `docs/` plus generated references | documentation link/inventory checks | archive legacy Caspar docs; restore from Git if needed | no contradictory authoritative inventory | Phase 9/10 |
| RL-018 | DELETE | tracked `dist/*` binaries/runtime blobs | build/install scripts | signed release/OCI artifacts | A001/A007 plus artifact parity | **Replacement workflow delivered, observation pending.** `.github/workflows/build-node.yml` now builds outside the source tree and emits deterministic archives, SPDX SBOMs, SHA-256 sums, and signed provenance/SBOM attestations under the checked A906 policy. Retain `dist/*` until both architectures have a successful tagged run, independent verification and scanner evidence are retained, consumers use promoted artifacts, and rollback is rehearsed. | builds/install no longer consume tracked blobs | Phase 9 |
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
| RL-001 | **Deleted (ADR 0004 closed 2026-09-28).** `apps/aseman-node` owns the implementation and composition (`app::NodeApp`); the `caspar-node` and `caspar-keygen` alias binaries are deleted, and retired Caspar configuration names are refused at load with the canonical key named. |
| RL-002 | **Partly moved; the wire families are migrated with compatibility shims.** The wire DTO families (`update`, `worker`, the `packet` types including the `multipart` `FileHeader`, and the chain wire DTOs) moved to `aseman_contracts::legacy_wire` and are re-exported from `apps/aseman-node/src/models/*` as pure re-export shims; the `compat/` module is deleted. The remaining `models/*` content is the legacy state/orchestration layer (`ICore`, `IState`, `ITrx`, the `I*` action and port traits) that the application use cases replace. Open until those transports retire (RL-009) and the orchestration replacement (RL-003) lands. The dead `worker` re-export shim (`models/worker.rs`) and the vestigial `app_pending_trxs` field/trait (a no-op placeholder on the `ICore` trait whose `Vec<WorkerTrx>` was never written or read) were deleted; grouped chain-transaction execution reached workloads as invocations (P5-06). |
| RL-003 | **Partly rewritten.** The use cases this migration needed are in `aseman-application`. `apps/aseman-node/src/core/*` keeps the compatibility orchestration the legacy actions still call. The former `legacy/` directory was renamed to `core/` to describe its node role; this is a path-only ownership clarification, not deletion-gate evidence or a change of state authority, and rollback is a source-path revert. Open with RL-004. |
| RL-004 | **Every A701 route executes through the migrated public executor.** The node composes the A701 gateway: `PublicActionExecutor` + `LegacySessionDirectory` (`apps/aseman-node/src/api/public_http.rs`) supply the two missing ports, and the TLS listener starts from `app::NodeApp` when `ASEMAN_PUBLIC_HTTP_*` is configured — observed live (RFC 9457 401/404/400 over TLS, contract routing). The finance family migrated into `aseman-application::finance` over the new `FinanceLedger` port, served from the legacy transaction by `api/model/finance_ports.rs` (ADR 0026 keeps finance on legacy until Phase 8); the entity/workload, identity session, creature signal/types, and program-list families route through the executor's port-bound bodies. `identity.signature.check` stays fail-closed by design (it required the legacy ROOT user, which a UUID subject cannot map to). Every action is registered (A402) and published (A701); the legacy handlers remain for the legacy transports until they retire (RL-009). |
| RL-005 | **Transformed into a provider choice (ADR 0033).** Storage is one selected provider per node: `postgres` (default) runs every transaction on PostgreSQL (capsules through the unit of work, the transitional surface through `PostgresTrx`) and opens no RocksDB; `rocksdb` is a supported, self-contained provider in `modules/storage/rocksdb`. The mixed PostgreSQL-plus-RocksDB commit is gone. QuestDB serves only the RocksDB provider's optional signal log (`ASEMAN_SIGNAL_LOG_PROVIDER`). The Caspar-data importers remain the one-way migration path. |
| RL-006 | **Replacement live; deletion gated on the legacy VMM runtimes.** Guest data routes to per-creature PostgreSQL databases through the trusted guest proxy (A306/A405), observed in the cutover. The legacy embedded VM database host calls stay until the legacy VMM runtime path retires (RL-013/ADR 0004); no workload-selected namespace or raw storage handle exists on the new path. |
| RL-007 | **Replaced.** Signing and verification are `aseman-identity-native` behind `IdentityVerifier` and `KeyDirectory`, proven by live identity tests (P4-01). `apps/aseman-node/src/adapters/security.rs` remains as the legacy adapter inside its ADR-0004 window. |
| RL-008 | **Replacement path composed; deletion evidence open.** The durable realtime provider, outbox, and checkpoints are delivered and proven live (P7-04, A707). The public SSE edge admits through `topic.subscribe`, binds the verified creature scope, replays from PostgreSQL with explicit resync, and bridge `publishUpdate` appends A707 plus its outbox row before compatibility fan-out. The WebSocket/A702 terminal edge binds its log stream to the authorized typed workload. `apps/aseman-node/src/adapters/signaler.rs` and process-local gateway subscribers remain for rollback until deployed parity/reconnect observation and ADR 0004 approval permit deletion. |
| RL-009 | **Retained as selectable transport modules (ADR 0033 decision).** HTTP (A701) is the default; the TCP and WebSocket adapters stay framing-only over the shared dispatch (`check_legacy_transports.py`) and are enabled per port. Their Caspar-named compatibility surface is removed with ADR 0004. |
| RL-010 | **Retained as a selectable federation transport.** Federation HTTP (mandatory mTLS, A401/A705) is the default and canonical workload actions route through it; the custom TCP federation transport stays available as an alternative module. |
| RL-011 | **Implementation, adapter, finance composition, and validator governance moved; live switch open.** `modules/consensus/hashgraph` owns the Babble engine, peer transport, its persistent event store (which, since ADR 0035, persists through the storage-neutral consensus-log port that the selected RocksDB or PostgreSQL provider implements, and depends on no database), all 213 original tests, and a `governance` subsystem (staking + weighted election) that the node's legacy globe used to own. `HashgraphConsensusProvider` implements the `ConsensusProvider` port over the engine's real application proxy: submissions enter Babble, committed blocks finalize records, pending work blocks adoption, and snapshots/checkpoints preserve the epoch state. Provider-specific properties (staking thresholds, unbonding window, election timing, validator cap) are configured environment-style through the new `ConsensusProvider::set(key, value)` method, read from `ASEMAN_CONSENSUS_*` variables, so the core never couples to one provider's feature set and diverse providers can be composed. The node composes the provider once in `load_inner`, stores it on `Core`, and both the finance flow (`aseman_application::consensus` + the executor's journal submission) and the legacy globe share it. The globe's own `staking.rs`/`election.rs`, its `Inner` staking/election ledger, the `Election`/`CoreState.elec_*` bookkeeping, and every staking/election type it carried (`STAKE_ACTION_*`, `PeersFn`, `election_meta`, `voter_id`, the `governance` field, the `handle`/`stake_node_owner`/`try_start_scheduled_election` controllers, and the `ChainPacketOp::Election` wire variant) were **deleted or moved**: the action constants now live in `governance::staking`, and the globe is a pure chain-RPC transport with no staking/election surface. `Governance` (in `modules/consensus/hashgraph`) is fully autonomous: `handle_stake`/`handle_election`/`next_stake_nonce` own the bond/unbond/slash rules, the commit/reveal round, and the weighted validator selection; it emits outbound election packets through its own chain edge; and `HashgraphConsensusProvider::spawn_election_scheduler` runs the hourly election on its own clock. The provider's `ConsensusHandler` **is the main chain's application handler**: `Blockchain::with_consensus` installs the provider's proxy into the main-chain Babble engine, so committed `stake`/`election` transactions are consumed by governance and finance records are finalized by the provider, while only `base`/`message`/`response` transactions are forwarded to the chain module's registered pipeline. The node chain module routes/submits only request/response/message, and the only way to modify staking/election is `ConsensusProvider::set` fed from `ASEMAN_CONSENSUS_*` environment variables. The node keeps its legacy `IChain` translation. A checkpointed switch and its rollback are now observed on a live local mesh (`modules/consensus/hashgraph/tests/live_mesh_handover.rs`: four validators over real TCP, identical finalization and checkpoints, stop, adoption by a second mesh, continued ordering, refused fork, restored prior order), and the unbounded RPC read that let a silent peer block `Node::shutdown` is fixed. Deletion waits on the same switch on a production peer mesh. The chain-module surface was also consolidated: the node's `callbacks`/`message_callbacks` correlation stashes and their mechanisms moved into the chain module (`IChain`/`Blockchain::register_chain_callback`/`park_chain_callback`/`take_chain_callback`/`register_message_callback`/`take_message_callback`), and the core-owned finance state (`free_nodes`, `CostConfig`, and the pay-lock/cost accessors) moved into a dedicated `core::orchestrator::finance` module owned by `Core` (`Core.finance`), exposed through the `ICore` cost/free-node accessors. The outbound chain submission queue (`chain_tx`/`chain_rx` and the framing drain) moved into the chain module as `IChain::submit_chain_op`, and the consensus provider is now owned by the chain module (`Blockchain.consensus`, exposed as `IChain::consensus_provider`) rather than the core orchestrator. |
| RL-015 | **Deleted (ADR 0004 closed).** `apps/asemanctl` is the only CLI; the `casparctl` binary, the Docker-era `install`/`start`/`pause`/`resume`/`stop`/`stats`/`purge`, the local `run`/`status`/`install --local`, `pprof`, and the owner bootstrap over the custodial login are deleted. `status`/`start`/`stop` act on the bootstrapped compact deployment. |
| RL-017 | **Archived and integrated.** The duplicate `wiki/` root is deleted, historical Caspar pages are status-labelled under `docs/legacy/caspar`, and `docs/README.md` is the sole current portal. Link and generated-inventory checks guard the result. |
| RL-019 | **Deleted.** The `/creatures/login` action, its login grants (`grantLogin` host call, `ASEMAN_LOGIN_MODE`), and its A402 action are removed; no Aseman surface returns account private keys. First-identity enrollment for a fresh deployment is the open follow-up. |
| RL-012 | **Moved into the RocksDB provider (ADR 0033, amending ADR 0012).** OpenRaft now lives in `modules/storage/rocksdb::cluster`: every write batch of the replicated store is a Raft entry applied in log order on every replica and acknowledged after this replica applied it; membership, the Raft RPC listener, and `asemanctl cluster` belong to the provider. The node's cluster adapter, its commit hooks, and the deploy-artifact replication are deleted; nothing above the storage seam proposes to Raft. Verified by a three-replica in-process cluster test over loopback HTTP. |
| RL-016 | **Deleted.** The combined image, its QuestDB-in-container entrypoint, and `sample.env` (`deploy/legacy`) are removed; separate images, compose profiles, `asemanctl bootstrap`, and `scripts/install.sh` replace them. |
| RL-018 | **Deleted.** Tracked `dist/` (539 MB) and `scripts/build-dist.sh` are removed and `dist/` is ignored. `.github/workflows/release.yml` builds every binary through `scripts/stage-release.sh`, scans, attests, and publishes a GitHub Release; `scripts/install.sh` installs it and downloads WasmEdge (and optionally Firecracker) verified against `contracts/release/runtime-dependencies.json`. |

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
  handoff planner (`modules/storage/rocksdb/src/vmhandoff.rs`); every instance is adopted
  into `core.workload` or explicitly stopped, with the operator's decisions bound to the
  export digest (P5-05, `docs/operations/vmm-handoff-runbook.md`). `ModalVolume` handles
  remain unadopted and stay the P6 runtime module's obligation.

RL-014 now has a dynamic process boundary. `aseman-vmm` accepts runtime-keyed A504
routes through `RoutingBackend`; a signed/supervised runtime backend process can be
installed or replaced without rebuilding the VMM service or node, while the default
backend remains the rollback path. The native compatibility backend still contains
its compile-time aggregate as the recovery fallback. Deleting that aggregate remains
behind deployed per-runtime parity and ADR 0004, not missing routing implementation.

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
