---
status: CURRENT
owner: migration
source_of_truth: the phase gate documents and docs/migration/acceptance.md
last_verified_commit: eebb9c5
verification: cargo xtask fast
---

# Where the migration stands

Written for: whoever picks this up next, cold.

## The one-paragraph version

The architecture is built and proven against real infrastructure — PostgreSQL, Docker,
Nomad, and Firecracker, not doubles. Phases 0 through 8 are accepted. The Phase 3
operator cutover to `ASEMAN_CORE_STORAGE_PROVIDER=postgres` has been performed and
observed on a development host; production-cluster observation and the legacy deletion
gate remain. Phases 9 and 10 are recorded as **partial**, honestly, because their criteria are observable outcomes
(a clean host bootstrapping in one command; every acceptance criterion passing) and
only part of them is observed: a compact deployment now bootstraps to healthy in one
command on a development host, a PostgreSQL backup restores onto a clean cluster, and a
checkpointed Hashgraph switch runs on a live local mesh. What remains is production
evidence — signed-image and clustered runs, whole-deployment restore, tagged artifact
promotion, production-shaped operational scenarios — and the legacy deletions that the
ADR-0004 compatibility window gates. None of it is blocked on design.

## Phase gates

| Phase | State | Gate |
|---|---|---|
| 0 Audit | ACCEPTED | `phase-0-gate.md` |
| 1 Workspace and contracts | ACCEPTED | `phase-1-gate.md` |
| 2 Module system | ACCEPTED | `phase-2-gate.md` |
| 3 Capsule storage | **ACCEPTED** | `phase-3-gate.md` |
| 4 Security and authority | ACCEPTED | `phase-4-gate.md` |
| 5 Extract the native VMM | ACCEPTED | `phase-5-gate.md` |
| 6 Nomad and worker topology | ACCEPTED | `phase-6-gate.md` |
| 7 HTTP, federation, realtime | ACCEPTED | `phase-7-gate.md` |
| 8 Finance and metering | ACCEPTED | `phase-8-gate.md` |
| 9 CLI, packaging, operations | **PARTIAL** | `phase-9-gate.md` |
| 10 Hardening and rollout | **PARTIAL** | `phase-10-gate.md` |

## What is left, in rough order of leverage

1. **The Phase 3 storage cutover has been performed.** A309 verification, the guest
   proxy, and the `ASEMAN_CORE_STORAGE_PROVIDER=postgres` switch all ran against a live
   PostgreSQL instance on a development host, and the node boots healthy on the capsule
   path. A production-deployed cluster observation and RL-005's deletion evidence are
   the remaining release items.
2. **Compose the node and stream the public contract.** `contracts/public/openapi.json` is
   generated and authoritative; `modules/network/http` provides its hardened TLS/HTTP
   transport boundary; `aseman-public-service` composes the authenticated/authorized
   action service with durable idempotency (A401 + A402 + `ActionExecutor` seam +
   `PublicActionIdempotency`), and the durable idempotency store is real over
   PostgreSQL (P7-06). **Every route in the generated A701 contract now executes
   through the node's migrated public executor (RL-004).** The finance family moved
   into `aseman-application::finance` over a new `FinanceLedger` port (`finance_ports.rs`
   serves it from the legacy transaction per ADR 0026); entity/workload, identity
   session, signal, types, and program-list route through the executor's port-bound
   bodies. `identity.signature.check` stays fail-closed by design (it required the
   legacy ROOT user, which a UUID subject cannot map to). Creature-scoped SSE now
   replays A707 from PostgreSQL with explicit retention resync, and bridge updates append
   durably before compatibility fan-out. The authorized WebSocket log-terminal stream,
   A702 RPC server, live A703 generation broker, and both federation directions are
   now composed. Canonical workload actions resolve the remote home node, authorize at
   source and destination, sign the request, and verify the descriptor-bound response.
   What remains is deployed default/rollback and rotation/partition observation
   (RL-009/RL-010); the old protocol remains only for ADR-0004 compatibility IDs.
3. **Finish `asemanctl` and packaging** (RL-015, RL-018). The canonical
   `apps/asemanctl` crate owns the command implementation and `casparctl` is a
   one-way warning compatibility shim. The `doctor`/`backup`/`restore`/`upgrade`/
   `support-bundle` administration groups are now implemented and drive the A902
   resumable journals (RL-015). A versioned contract gate-checks the complete dispatch
   set, output modes, exact exit-code meanings, and global redacted JSON envelopes.
   Generated TypeScript/Python clients cover all 76 public operations. Clean-host and
   cluster observations plus the ADR-0004 compatibility window remain incomplete.
   `asemanctl bootstrap` now drives the executable compact profile with resumable state
   and off-tree secret generation; an executable three-replica cluster service profile
   consumes operator-owned HA PostgreSQL, Nomad, and a stable load balancer. Separate
   least-privilege image definitions for node, VMM, meter, and Nomad backend live under
   `deploy/images`. The checked A906 workflow now builds out of tree and emits
   deterministic archives, SPDX SBOMs, checksums, and signed attestations. Tracked
   `dist/*` blobs stay until a successful tagged run, scanning, independent verification,
   consumer cutover, and rollback evidence complete RL-018's second gate.
4. **Observe the Hashgraph financial epoch switch in production** (RL-011). A
   checkpointed switch and its rollback now pass on a live four-validator local mesh
   over real TCP (`live_mesh_handover`), which also exposed and fixed an unbounded RPC
   read that let a silent peer wedge `Node::shutdown`. The complete engine now
   lives in `modules/consensus/hashgraph`; its `HashgraphConsensusProvider` sends
   records through the real Babble proxy and derives finalizations/checkpoints from
   committed blocks. The node now composes this provider into its finance flow: the
   migrated finance actions offer each written journal record for ordering through the
   `ConsensusProvider` port (`aseman_application::consensus`), and the composition
   installs a `HashgraphConsensusProvider`. The same switch on a production peer mesh
   remains.
5. **Execute the operational scenario manifest at production shape.** Parser property
   fuzzing, public-action load-lite, and focused PostgreSQL realtime/metering load/soak
   runs pass. A checked nine-scenario manifest and HTTP load probe define numeric
   thresholds; scheduling/federation/guest/HTTP scale, longer soak, canary, and rollback
   reports still need the intended deployment.
6. **The remaining legacy deletions.** Every one is a row in the removal ledger with its
   blocker named, and `scripts/check_removal_ledger_due.py` fails a release if one goes
   quiet.
7. **Retire superseded roots after their gates.** The generated
   `docs/generated/repository-layout.md` compares the plan to the tree. Every planned
   target package path now exists and owns implementation source. The generated layout
   currently reports one legacy root, `dist`, behind its publication gate; compatibility
   code also remains inside canonical packages until its named removal gates pass. Path completion is
   not treated as permission to delete them.

Delivered in the 2026-09-28 pass: the node's crate-wide `allow(dead_code)` became
scoped, self-retiring `expect` attributes; PostgreSQL-aware, signature-trusted,
resumable backup/restore with a clean-cluster drill; the Hashgraph mesh handover and
its transport fix; `ASEMAN_SIGNAL_LOG_PROVIDER=postgres`; and the first real compact
bootstrap, which found and fixed eight profile/driver defects (see the Phase 9 gate).

Delivered in the previous pass: the requirements traceability report (A1005) runs in
`cargo xtask fast` and records 25 requirements, 0 violations, 16 MET, 9 PARTIAL, 0
OPEN. The repository-structure deviations from the plan's proposed hierarchy are
documented below.

## How to check the state yourself

```text
cargo xtask fast     # architecture, generated-artifact freshness, tests, clippy
cargo xtask full     # plus the legacy node's own suite and the native backend
```

The live suites need real services and skip without them:

```text
ASEMAN_TEST_POSTGRES_URL=...   PostgreSQL: storage, identity, coordination,
                               realtime, federation, finance, two-cluster federation
ASEMAN_TEST_NOMAD_ENDPOINT=... Nomad: the Nomad backend and worker lifecycle
                               (Docker also required)
```

The worker agent's suite skips without a `firecracker` binary. No suite fakes a service
it could have used for real.

## Reading order for a cold start

`plan/migration/15-agent-execution-guide.md`, then `docs/migration/acceptance.md` for
what is and is not done, then the gate document of whatever phase you are touching, then
its work units in `docs/migration/work-units/`, then the ADRs it names.

## Repository structure vs. the plan's proposed hierarchy

`plan/migration/01-target-architecture.md` and `13-clean-code-structure-and-deletion.md`
propose a **target** hierarchy. The current tree deliberately differs in these ways:

- **The planned nested capability packages now own their implementations.** Storage,
  network, federation, realtime, security, finance, consensus, VMM backends, and
  runtime drivers are at the paths in plan 01. Older auxiliary modules such as
  `guest-http`, `identity-native`, `public-service`, and the VMM protocol crates remain
  one level deep because they are additional adapters, not substitutes for a missing
  planned package.
- **`crates/aseman-capsule` owns the capsule repositories.** The capsule protocol lives
  in `aseman-contracts::capsule`; no compatibility repository crate proxies it.
  `aseman-guest-sdk` now owns workload-bound signed request construction without any
  caller-selected tenancy API.
- **All planned `apps/` roots own their implementations.** The `caspar-node`,
  `caspar-keygen`, and `casparctl` ADR-0004 warning aliases are binary targets inside
  the canonical app packages; no proxy package or reverse dependency remains. The
  node composition root is `app::NodeApp` in `apps/aseman-node/src/app.rs`, and the
  dead `bots/` demo tree was removed from the node crate.
  `apps/aseman-meter` is independent and composes A501 collection with PostgreSQL
  usage, pricing, and ledger ports.
- **Legacy roots were consolidated without proxy crates:** the client is
  `apps/aseman-client`, all seven runtime
  implementations plus their compatibility SDK are `modules/runtime`, the generated
  registry belongs to the native backend, and old wiki pages are explicitly archived
  in `docs/legacy/caspar`. Tracked `dist/` remains open under RL-018.
- **The required ownership roots now exist.** `deploy/` owns checked compact and cluster
  profiles tied to the authoritative
  topology contract, creature-implementation guidance lives in
  `docs/development/creature-implementation.md`, generated public clients live in
  `apps/aseman-client/generated/`, and `tests/evals/agent/` contains a mechanically
  checked cold-start catalog. Root
  `ARCHITECTURE.md`, `CONTRIBUTING.md`, `SECURITY.md`, `CHANGELOG.md`, and the
  `docs/` portal are present. Clean-host compact observation, broader generated SDK
  examples, and deployment-backed evaluation runs remain Phase 9 work.

In short: the plan's hierarchy is the target; the tree is the strangler's current edge.
The dependency boundaries the plan actually enforces (`aseman-domain` ← `aseman-ports`
← `aseman-application`, modules behind contracts) are checked by `cargo xtask arch` and
are met. The exact structural differences are generated from
`contracts/repository/layout.json`; the missing/renamed entries are each a named
removal-ledger or Phase 9/10 obligation.
