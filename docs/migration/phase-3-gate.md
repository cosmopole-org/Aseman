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
| Program, gateway, entities, VM resource stores, chain, federation state | open | — | — |

No node code outside `store_ports.rs` and `model/access.rs` reads or writes an
`onaccess`/`hasaccess` key; the grep in the P3-06 record checks this. The A004 scanner
recognizes the shared `access_link_key` builder, so membership stays inventoried and
the A308 manifest keeps zero blocked rows.

## Next steps

1. Rewire the remaining action families onto application use cases with repository
   ports, adding legacy and capsule adapters under the same characterization tests.
   Stores, membership, and the creature family (identity, balances, metadata, types,
   owner links) are done. The finance ledger stays on legacy until P8 and identity
   credentials until P4 (ADR 0026). Program, gateway, entities, VM resource stores,
   chain, and federation state remain. LD-14 (unauthorized VM
   creature host calls) must be fixed before cutover.
2. Add typed storage-provider selection and route authoritative reads and writes by
   binding generation (A309 cutover), per-family as ADR 0026 routes them.
3. Re-evaluate this gate.
