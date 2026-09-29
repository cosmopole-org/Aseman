---
status: DECISION
owner: architecture/storage
source_of_truth: this ADR; supersedes ADR 0026 (dual-provider routing) and ADR 0031 (compatibility transactions); amends ADR 0033 and ADR 0035 (consensus-log names)
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
   natural string key. A record's id is `derived_capsule_id(family, key)`
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

5. **Migration is explicit.** `asemanctl storage migrate [--to <provider>]` runs
   while the node is stopped (in-process for a host node, as `aseman-node storage
   migrate` in a one-off container for the compact deployment). The engine is
   `aseman_storage_providers::migrate`:
   - *Legacy conversion.* A store from before this ADR — the RocksDB key/value base
     or PostgreSQL's `aseman_compat` schema — is read as legacy physical records and
     transformed by the reviewed A308/A309 transform (fail closed). Capsules import
     verbatim; a keyed model gains its natural `key` from its legacy identity. Guest
     pairs become `core.guest_pair` rows, the legacy finance epoch is kept as
     `finance.legacy_record` (ADR 0017) and bridged into the live finance models
     (documents, markers, wallet counters, pool links, journal participants), the id
     counters carry over, and the legacy signal history (QuestDB or
     `aseman_legacy_log.signals`, `--signal-log`) becomes `realtime.event` streams.
     Evidence for on-disk artifacts is gathered from the storage root;
     `--file-artifact ID=PATH` supplies legacy `File` bytes.
   - *Provider copy.* With `--to` naming another provider, every model's capsules
     (tombstones and revision chains included) and every consensus log copy into an
     empty target and are verified by count and digest. The source stays unchanged as
     the rollback.
   - A conversion done in place retires the legacy layout
     (`StorageProvider::retire_legacy_layout`): the RocksDB base and old log
     directories and PostgreSQL's `aseman_compat` schema are renamed aside, kept for
     inspection. A node refuses to start while a provider reports a legacy layout and
     names the command.

6. **Consensus logs have relative names.** A log is named `chains/{work_chain}/{shard}`
   on every provider (the RocksDB provider keeps them under
   `{storage_root}/consensus`), and `ConsensusLogStorage::names` lists them for the
   migration. Logs Hashgraph kept under absolute directories are legacy layout and
   move with the conversion.

7. **One model per concept.** Where a legacy transform and the node could disagree,
   the node uses the transform's model: secrets are `core.creature_secret` and
   `core.secret_grant`, emails are `core.user.email`, token locks are
   `core.token_lock` (behind the finance ledger's creature documents too), and chains
   are `core.chain`/`core.chain_shard` with their natural keys. Legacy session tokens
   are kept only as digests (`core.session_token`); sessions from before the
   migration are revocation markers in `core.session`.

8. **Guest data on every provider.** Without a PostgreSQL guest data plane, the node's
   storage serves every creature's guest pairs and confined documents
   (`StorageGuestKv`, `core.guest_pair`) with the same semantics as a creature's own
   guest database.

## Consequences

- `ITrx`, `StateBackend`, both transaction adapters, and the dual-path routing of
  ADR 0026 are deleted. PostgreSQL no longer creates `aseman_compat` (existing
  databases keep it until `storage migrate` retires it) and drops the replaced
  `replay_nonces`, `identity_challenges`, and `public_idempotency` tables, whose
  entries were short-lived.
- The node no longer writes QuestDB or the PostgreSQL signal-log tables;
  `ASEMAN_SIGNAL_LOG_PROVIDER` only tells the migration where the legacy history is.
- Families no reviewed transform exports and no node code wrote are not models: the
  hand-written `god::` superuser flag (ADR 0020; elevated authority is a capability
  grant), `NodeIpToHost` links, federated member metadata that nothing read,
  `UserPrivateKey`, and consumed-token markers.
- Families that had only derived links (`creatorof`, `ownerof`, `machinePrograms`,
  email maps, finance listings) become relations and indexes of their models.
- Adding a provider is a plugin plus a passing conformance run; node code does not
  change.
