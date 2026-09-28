---
status: DECISION
owner: architecture/storage
source_of_truth: this ADR; supersedes ADR 0026 (dual-provider routing) and ADR 0031 (compatibility transactions); amends ADR 0033
last_verified_commit: pending
verification: aseman-storage provider conformance suite against every provider plugin; node suites on each provider; asemanctl storage migrate drills
---

# ADR 0036: One storage module, plugin providers, and a model API

## Status

Accepted 2026-09-28.

## Context

The node reached storage through two paths: typed capsule ports on PostgreSQL
(ADR 0026) and the legacy `ITrx` key/value transaction everywhere else. `ITrx` is
RocksDB-shaped (`put_link`, `get_links_list`, `put_obj`, `get_json` …), about 480 call
sites in 53 node files, and each provider needed its own adapter
(`adapters/rocksdb/trx.rs`, `adapters/postgres/trx.rs`, the `aseman_compat` schema).
Node code named provider types, and a RocksDB node could not use the capsule store.

## Decision

1. **One storage module.** `crates/aseman-storage` defines the storage API; the node's
   `adapters/storage` module is the only code in the node that opens storage, and
   `core::trx` is the only transaction type node code uses. No other node module names
   a provider, a driver, or a key layout.

2. **Models, not keys.** Every persisted family is a *model* declared in the
   provider-neutral schemas (`contracts/capsule/kinds/*-logical-schemas.json`): typed
   fields, required fields, unique and range indexes, relations, and an optional
   natural string key. A record's id is `deterministic_legacy_capsule_id(family, key)`
   for keyed models (so migrated A308 data keeps its ids) and a fresh UUIDv7
   otherwise.

3. **A Prisma-style API.** The operations are `find_unique`, `find_first`, `find_many`
   (where / order by / skip / take / select), `count`, `create`, `update`, `upsert`,
   `delete`, `update_many`, and `delete_many`. Filters are `equals`, `not`, `in`,
   `not_in`, `lt`, `lte`, `gt`, `gte`, `contains`, `starts_with`, `ends_with` (optionally
   case-insensitive), `is_null`, and `AND` / `OR` / `NOT`. A generator emits a typed
   Rust client per model from the schemas (`trx.store().find_many(…)`); a wrong field
   or type does not compile.

4. **Providers are plugins.** A provider implements `StorageProvider` (transactions
   over models, its consensus-log storage, schema migration, and export/import for
   migration) and registers a `ProviderPlugin` under its name. The node loads the
   plugin named by `ASEMAN_CORE_STORAGE_PROVIDER`, which `asemanctl` sets, the way it
   routes VMM runtimes. Plugins are linked in one bundle crate
   (`aseman-storage-providers`) that only the node's composition names. Every plugin
   passes the same conformance suite.
   - PostgreSQL compiles the model API to SQL over its mapped tables (both layouts of
     ADR 0034, single database or sharded cluster of ADR 0033).
   - RocksDB runs it on its capsule store with secondary indexes per unique index,
     range index, and relation, and a planner that picks an index for equality,
     prefix, and range filters and for ordering, falling back to a bounded scan.
     Transactions buffer writes, read their own writes, and commit with the
     conditional batch (replicated by Raft in cluster mode).

5. **Migration is explicit.** `asemanctl storage migrate --from <provider> --to
   <provider>` copies every model and every consensus log between plugins, converting
   a legacy key/value store with the reviewed A308 transforms on the way. A node
   refuses to start on a store in the legacy layout and names the command.

## Consequences

- `ITrx`, `StateBackend`, both transaction adapters, the `aseman_compat` schema, and
  the dual-path routing of ADR 0026 are deleted once every caller is on the model API.
- Families that had only derived links (`creatorof`, `ownerof`, `machinePrograms`,
  email maps, finance listings) become relations and indexes of their models.
- Adding a provider is a plugin plus a passing conformance run; node code does not
  change.
