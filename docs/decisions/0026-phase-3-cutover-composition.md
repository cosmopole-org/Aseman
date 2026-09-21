---
status: DECISION
owner: storage/migration
source_of_truth: this ADR
last_verified_commit: a3212a7
verification: conformance suites on both adapters; live_creature_ports_pass_conformance_on_capsules; live_units_of_work_are_atomic_isolated_and_fenced; live_actions_route_core_families_to_postgres_with_ordered_commits; phase-3-gate.md
---

# ADR 0026: Phase 3 cutover routes each port family to one authoritative provider

## Status

Accepted 2026-09-21. It defines what "PostgreSQL is the default" means at the Phase 3
gate, given ADR 0017 (finance stays legacy-authoritative until P8) and ADR 0022 (VMM
observed runtime stays in the legacy provider until RL-013).

## Context

The RL-004 strangler moves node state behind application ports, one family at a
time. Each port has a legacy adapter and a capsule adapter. A309 cuts over by binding
generation, and the gate needs one authoritative provider per piece of state.

Accepted decisions keep some state on the legacy provider after Phase 3:

- ADR 0017 keeps the legacy finance subsystem authoritative until P8. That covers
  balances, counters, journals, holds, pools, and payouts. PostgreSQL holds its
  epoch checkpoint as read-only comparison and rollback data.
- ADR 0022 keeps VMM observed runtime in the legacy provider until RL-013.
- ADRs 0019 and 0023 attach P4 obligations to identity credentials before the legacy
  store stops being authoritative for them.

A creature's balance changes in the same legacy transaction as its withdrawable, debt,
and journal records. Moving balances to PostgreSQL while those records stay on legacy
would split one atomic change across two stores.

## Decision

1. **Routing unit.** Cutover routes whole port families, never single keys. The
   binding generation that A309 switches selects, for each family, the one provider that
   is authoritative for reads and writes.
2. **Phase 3 routing.**

   | Port family | Authoritative after the Phase 3 cutover |
   |---|---|
   | `StoreDirectory`, `StoreAccess`, `SignalLog`, `CreatureDirectory`, and every later core family | PostgreSQL capsules |
   | `CreatureBalances` and the finance ledger | Legacy provider until P8 (ADR 0017) |
   | VMM observed runtime | Legacy provider until RL-013 (ADR 0022) |
   | Legacy id allocation (`globalIdCounter`, `localIdCounter`) | Legacy provider until P4 issues UUIDv7 identities (ADRs 0009, 0020). Continuing the one counter cannot reuse an id |
   | Work chains and chain shards (`Chain`, `ChainShard`) | Legacy provider until P8, with the consensus provider (ADR 0025). Legacy writes them and never reads them back (LD-22), and shard ids collide across chains (LD-23), so P8 fixes both with its checkpoint contract |
   | Identity credentials: sessions, email login links, custodial private keys (LD-01), secrets, and login grants | Legacy provider until the P4 cutover obligations of ADRs 0019 and 0023 are met: proof of possession or accepted verification-only keys, secrets re-wrapped with AAD, and grants denied |

   The "PostgreSQL is the default" gate clause holds when every family outside these
   exceptions is served by capsules.
3. **Ports are split along provider boundaries.** A port never spans two families with
   different providers. That is why `CreatureDirectory::create` does not take a balance,
   and a balance is opened and closed through `CreatureBalances::open` / `close`.
4. **One unit of work per action, ordered commits.** When the core families are on
   PostgreSQL, every state action runs inside one PostgreSQL transaction (a unit of
   work) next to its legacy transaction.
   - The action's capsule reads and writes share that transaction, so it reads its
     own writes and a refused action rolls back on both providers.
   - PostgreSQL commits first, then legacy.
   - If the legacy commit fails after PostgreSQL committed, the compensations the
     action registered run in a new unit of work. The creature router registers them:
     a created identity is deleted again, and a deleted one is revived.
   - Other actions that touch both providers write only legacy state whose loss after
     a PostgreSQL commit is harmless: an id-counter gap, or VMM observed runtime that
     P5 reconciles.
   - Each family has one router between callers and its provider (`CreaturePorts`,
     `ProgramPorts`, `StorePorts`, `MembershipPorts`, `SignalPorts`,
     `GatewayPorts`). The legacy adapters are private to it.
   - Selection: `ASEMAN_CORE_STORAGE_PROVIDER` (`legacy` by default, or `postgres`
     with `ASEMAN_DATABASE_URL_SECRET`), and `ASEMAN_CORE_BINDING_GENERATION` fences
     every capsule write (A309).
5. **Prerequisite: LD-10 and LD-15.** A cross-provider action can compensate only if
   it sees a failed legacy commit, and a failed action must not commit partial writes.
   Both are fixed: `ITrx::commit` returns write errors, and secured actions discard a
   failed action's writes and fail on a failed commit.
6. **Read-your-writes.** Within one action, a read of a family goes to that family's
   authoritative provider, even when an earlier step wrote a different provider.

## Consequences

- Finance code keeps using the legacy transaction for ledger records, and uses
  `CreatureBalances` for balances. P8 moves both together.
- The capsule `CreatureBalances` adapter (`finance.wallet`) is conformance-tested now.
  It becomes authoritative only when P8 switches the finance family.
- The gate document lists each family's routing and states the exceptions.

## Rejected alternatives

- Moving balances with creature identity: this would split one atomic finance change
  across two stores before P8 defines the ledger.
- One port that covers identity and balance: that port could not be served by two
  providers, so it would force one of the two decisions above to be reversed.
- Two-phase commit across RocksDB and PostgreSQL: the legacy store has no prepare
  phase. Ordered writes with compensation are sufficient for the two actions that cross.
