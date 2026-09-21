---
status: IN_PROGRESS
owner: migration/phase-3
source_of_truth: plan/migration/09-migration-phases.md
last_verified_commit: a3212a7
verification: cargo xtask fast; cargo test -p caspar-node --lib; live aseman-storage-postgres and aseman-migration-e2e on PostgreSQL 16
---

# Phase 3 exit gate

## Decision

**Not yet accepted.** Every Phase 3 artifact (A301–A310) is accepted or verified, and the
migration protocol is proven end to end. The gate still requires the node to serve core
state from PostgreSQL, which depends on rewiring the legacy actions onto typed capsule
repositories family by family (RL-004 strangler). That work is open.

## Clause-by-clause status

| Gate clause | Status | Evidence / remaining work |
|---|---|---|
| All persistent classes use capsules (except ADR 0022 VMM observed runtime) | **Met** | A308 accepted with zero blocked rows (ADRs 0016–0025). Every class the export emits has a native writer: core, finance, telemetry, realtime, and guest KV per creature |
| PostgreSQL is the default (ADR 0026: every port family except balances and the finance ledger, which ADR 0017 keeps on legacy until P8, and identity credentials, which ADRs 0019/0023 keep on legacy until P4) | **Open** | The node still serves every family from the legacy provider. Remaining: port the families still read directly, add typed provider selection, and switch the binding generation. LD-10 and LD-15 (commit and action-failure atomicity) are fixed, which ADR 0026 compensations require |
| Each creature's guest records and schemas live in its isolated database | **Met for migrated state** | A306 isolation tests, and the ADR 0021 importer writes only into the owner's database. Live guest access switches with the P4-04 gateway |
| Legacy and PostgreSQL providers pass conformance, isolation, pool-contamination, schema, and migration/restart tests | **Partly met** | PostgreSQL: storage conformance, guest isolation, document capsules, fencing, and the A309 end-to-end test. Legacy: its export/KV/time-series seams and the LD-12 membership repair are tested (42 tests), but it is not a capsule repository, so the capsule conformance suite does not apply until action families read through repositories |
| Node/application crates no longer import RocksDB or QuestDB types | **Met, with two owned exceptions** | The core path uses `LegacyKvStore` and `QuestDbTimeSeries` from `aseman-storage-legacy`, and all 382 node library tests pass. Exceptions: the OpenRaft store (RL-012, ADR 0012) and the Hashgraph store (RL-011, ADR 0025) |

## Cutover granularity

Store state is not private to `/stores/*`. Membership links are also read by the
security guard (`hasaccess`), the signaler's fan-out (`onaccess`), creature deletion,
federation, and VM host calls. Switching one action family to PostgreSQL would therefore
split authority across readers. A309 switches one binding generation atomically, so the
node cuts over as a whole once **every** reader and writer of migrated state goes
through application ports. ADR 0026 routes each port family to one authoritative
provider: creature balances and the finance ledger stay on legacy until P8, so the
finance ledger is not ported in Phase 3. Creature create and delete cross the two
providers and compensate on a failed legacy commit. Until then, each ported family runs on its legacy adapter,
and the capsule adapter is proven against the same use cases.

## Strangler progress

| Family | Use cases | Legacy adapter | Capsule adapter |
|---|---|---|---|
| `/api/*` diagnostics and auth | Phase 1 | yes | n/a (stateless) |
| `/stores/*` (signal, history, setAccess, getAccess) | `aseman-application::store` | `LegacyStorePorts` (node test) | `aseman-capsule-repositories::store` (live PostgreSQL test) |
| Store membership, every reader and writer: guard, signaler fan-out, WS/TCP session join, VM run checks, program bootstrap, creature signal and delete, federation mirroring, VM host calls (create/delete store, createAccess/deleteAccess, list, listMembers, creature delete) | `StoreAccess` port | `LegacyMembership` (trx only; node test, including LD-12) | same `StoreAccess` implementation as above |
| Creature identity (create, get, list, update, delete, meta, getByUsername, find, `/machines/list`, VM creature host calls, route lookup) | `aseman-application::creature` | `LegacyCreatures` (node conformance test) | `aseman-capsule-repositories::creature` (live PostgreSQL conformance test) |
| Creature balances (transfer, mint, lock tokens, finance actions, reconciliation) | `CreatureBalances` port | `LegacyCreatures::account` (node test) | `finance.wallet` via `aseman-capsule-repositories::creature` (live conformance test) |
| Finance ledger (journals, withdrawable, debt, holds, payouts) | P8 (ADR 0017/0026) | stays legacy-authoritative | checkpoint only |
| Creature metadata (`CreatMeta`, `UserMeta`) | `CreatureMetadata` port | `LegacyCreatures` (node conformance test) | `core.creature_metadata` / `core.user_metadata` (live conformance test) |
| Creature type registry | `CreatureTypes` port | `LegacyCreatures` (node conformance test) | `core.creature_type` (live conformance test) |
| Identity credentials (sessions, email login links, custodial keys, secrets, login grants) | P4 (ADRs 0019/0023/0026) | stays legacy-authoritative | revocations/verification only |
| Programs and program metadata (`Program`, `machinePrograms`, `ProgMeta`) | `aseman-application::program`, `ProgramDirectory`, `ProgramMetadata` | `LegacyPrograms` (node conformance test) | `core.program` / `core.program_metadata` (live conformance test) |
| Program alarms (`vmAlarm*`) | `ProgramAlarms` port | `LegacyPrograms` (node conformance test) | `core.program_alarm` (live conformance test) |
| Store records and metadata (`Store`, `creatorof`, `StoreMeta`) | `StoreDirectory`, `StoreMetadata` | `LegacyStores` (node conformance test) | `core.store` / `core.store_metadata` (live conformance test) |
| Gateway routes (`vmHttpRoute*`) | `GatewayRoutes` port | `LegacyGatewayRoutes` (node conformance test) | `core.gateway_route` (live conformance test; pins not persisted, ADR 0022) |
| VM resource stores (`Json::VmResourceStore`, `vmOwnedStore`) | `VmResourceStores` port | `LegacyPrograms` (node conformance test) | `core.vm_resource_store` (live conformance test) |
| Legacy id allocation (`globalIdCounter`, `localIdCounter`) | P4 UUIDv7 identities (ADRs 0009/0020/0026) | stays legacy-authoritative | verified, not migrated |
| Work chains and shards (`Chain`, `ChainShard`) | P8 with the consensus provider (ADRs 0025/0026; LD-22, LD-23) | stays legacy-authoritative | checkpoint only |
| Entities, entity configs and artifacts, VM resource entities, files | blob-backed; ADR 0027 pending | — | — |

No node code outside `store_ports.rs` and `model/access.rs` reads or writes an
`onaccess`/`hasaccess` key; the grep in the P3-06 record checks this. The A004 scanner
recognizes the shared `access_link_key` builder, so membership stays inventoried and
the A308 manifest keeps zero blocked rows.

## Next steps

1. Blob-backed families (entities, entity configs and artifacts, VM resource
   entities, files) need the target blob store decided (ADR 0027, pending). Every
   other core family already reads and writes through ports with both adapters, and
   the families ADR 0026 routes to legacy (finance ledger and balances, identity
   credentials, VMM observed runtime, work chains, legacy id allocation) stay there.
   LD-14 (unauthorized VM creature host calls) and LD-24 (VM guests write arbitrary
   node keys) must be fixed before cutover.
2. Typed provider selection and per-action routing are in place (ADR 0026;
   `ASEMAN_CORE_STORAGE_PROVIDER`, `ASEMAN_CORE_BINDING_GENERATION`). The cutover
   itself is an operator switch after A309 verification, blocked by LD-14, LD-24, and
   the blob-backed families.
3. Re-evaluate this gate.
