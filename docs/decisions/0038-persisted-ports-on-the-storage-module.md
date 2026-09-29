---
status: DECISION
owner: architecture/storage
source_of_truth: this ADR; extends ADR 0036 to every persisted port
last_verified_commit: pending
verification: cargo xtask fast; cargo xtask full (live PostgreSQL); crates/aseman-storage-providers/tests/{coordination,realtime,finance,federation,vmm}.rs on memory, RocksDB, and PostgreSQL
---

# ADR 0038: Every persisted port is an adapter on the storage module

## Status

Accepted 2026-09-29.

## Context

ADR 0036 made the storage module the one door to the node's database, but five port
families still kept their own PostgreSQL-only adapters with hand-written SQL and
migrations of their own:

| Port | Former adapter | Former tables |
| --- | --- | --- |
| Coordination (A607) | `aseman_storage_postgres::coordination` | `aseman_core.coordination_lease`, `coordination_fence` |
| Realtime log, outbox, checkpoints (A707) | `aseman-realtime-durable` | `aseman_core.realtime_event`, `realtime_outbox`, `realtime_checkpoint` |
| Metering, pricing, ledger (Phase 8) | `aseman-finance-ledger` | `aseman_core.usage_sample`, `usage_interval`, `journal_record`, `journal_entry`, `price_list` |
| Federation directory and envelope guard (A704/A705) | `aseman_federation_http::store` | `aseman_core.federation_node`, `federation_workload`, `federation_nonce`, `federation_answer` |
| VMM service stores (A501/A503) | `aseman_storage_postgres::vmm` | `aseman_vmm.*` |

A node on the RocksDB provider could therefore not run any of them, each adapter
carried its own pool, migration, and error mapping, and their tables were outside the
provider's layout, sharding, and migration tooling.

## Decision

1. **One adapter per port, on the storage module.** Each port is implemented once in
   `aseman-capsule` (`coordination`, `realtime`, `metering`, `federation`, `vmm`) over
   typed models declared in `contracts/capsule/kinds/` (`core.coordination_lease`,
   `core.realtime_log_event`, `core.usage_sample`, `core.vmm_workload`, …). Every
   provider plugin serves them; no port adapter names a provider.
2. **Decisions are optimistic transactions.** `AutoCommit::decide` runs a decision in
   a read-write transaction; every update carries the revision it read, and a commit
   another writer won is retried (at most `MAX_CAS_ATTEMPTS`). A refusal the decision
   itself made is returned and never retried. This replaces `SELECT … FOR UPDATE`,
   `SKIP LOCKED`, and `ON CONFLICT` in the former adapters; unique indexes turn racing
   first writes into a conflict for the loser.
3. **Time-based decisions use the provider's clock.** `StorageProvider::now_millis`
   is the host clock by default; the PostgreSQL provider answers with the database's
   `clock_timestamp()`, so every replica decides lease expiry on one clock, as the
   former adapter did.
4. **Sequences are counters.** The VMM event log's sequence and truncation point are
   `core.counter` rows written in the same transaction as the event; appends and
   truncation serialize on the counter's revision.
5. **Existing data is imported, then retired.** `aseman_storage_providers::open`
   imports the former tables into the models when a PostgreSQL storage opens
   (`port_tables`), resuming by key after an interruption, and moves each imported
   table unchanged into the `aseman_retired` schema. The VMM service and the meter open
   their database through `aseman_storage_providers::open_database`.

## Consequences

- The node's realtime and federation stores use the node's own storage; the VMM
  service and the meter use the PostgreSQL provider on their own databases.
- `aseman-realtime-durable`, `aseman-finance-ledger`, the federation store, and the
  PostgreSQL provider's coordination and VMM modules and their migrations are removed.
- Rollback: the retired tables are the former adapters' data as of the import. Rolling
  back to a release before this ADR requires moving them back from `aseman_retired`;
  writes made after the import are not carried back.
- Deletion gate for `aseman_retired`: one release after every deployment reports a
  completed import (RL row "persisted ports on the storage module").
