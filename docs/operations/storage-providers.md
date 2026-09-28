---
status: CURRENT
owner: architecture/storage
source_of_truth: docs/decisions/0033-distributed-storage-providers.md and docs/decisions/0034-capsule-layouts.md, docs/decisions/0035-consensus-log-port.md
verification: cargo test -p aseman-storage-postgres --test live_sharded --test live_document_capsules; cargo test -p aseman-storage-rocksdb --lib cluster:: capsule_store
---

# Storage providers

A node runs on exactly one storage provider, chosen with
`ASEMAN_CORE_STORAGE_PROVIDER`. Each provider distributes itself; nothing above the
storage seam knows whether it runs on one host or a cluster (ADR 0033).

| Provider | Single host | Cluster mode |
|---|---|---|
| `postgres` (default) | one PostgreSQL database (`ASEMAN_DATABASE_URL_SECRET`) | shards with replicas (`ASEMAN_POSTGRES_SHARDS_SECRET`) |
| `rocksdb` | embedded RocksDB under the storage root | replicas kept identical by OpenRaft (`asemanctl cluster`) |

## Consensus logs

The Hashgraph engine persists its log through the consensus-log port (ADR 0035), so
consensus data lives in the selected provider: one embedded RocksDB database per shard
log under the data directory, or rows of `aseman_consensus.log_entries` in the home
PostgreSQL database. The engine does not know which.

## Capsule layout

`ASEMAN_STORAGE_CAPSULE_MODE` chooses how both providers lay out an entity (ADR 0034):

| Mode | PostgreSQL row | RocksDB keys |
|---|---|---|
| `off` (default): flattened | every field is its own column; a document field is a JSONB column; `capsule_cbor` is empty | one key per field plus a metadata key |
| `on`: capsule mode | the signed envelope is packed into `capsule_cbor` beside the typed columns | one key holding the envelope |

Reads return the exact signed capsule in both layouts and refuse a row that was edited
outside the provider. The node records the layout in the database when it migrates;
changing the setting and restarting the node rewrites the mutable rows into the new
layout (append-only rows stay as they are and remain readable). The provider adds and
retypes columns as the mapping changes; it never drops one.

## PostgreSQL

Single host: set `ASEMAN_DATABASE_URL_SECRET` (and the guest proxy settings). The node
migrates the schema on start.

Cluster mode: `ASEMAN_POSTGRES_SHARDS_SECRET` names a JSON shard map, and
`ASEMAN_DATABASE_URL_SECRET` names its home shard's primary:

```json
{
  "version": 1,
  "home": "eu-1",
  "read_from_replicas": true,
  "shards": [
    {
      "name": "eu-1",
      "primary": "postgresql://aseman:…@pg-eu-1:5432/aseman",
      "replicas": ["postgresql://aseman:…@pg-eu-1-r1:5432/aseman"],
      "guest_proxy": "postgresql://aseman_guest_proxy:…@pg-eu-1:5432/postgres"
    },
    {
      "name": "us-1",
      "primary": "postgresql://aseman:…@pg-us-1:5432/aseman",
      "replicas": [],
      "guest_proxy": "postgresql://aseman_guest_proxy:…@pg-us-1:5432/postgres"
    }
  ]
}
```

- Every shard needs `max_prepared_transactions > 0`: writes that span shards commit
  with two-phase commit, and the node resolves transactions a crash left prepared
  when it starts.
- Reference kinds (referenced ones, and ones with uniqueness beyond their id) are on
  every shard; the high-volume telemetry kinds are spread by hash. Adding a shard
  moves only the rows the new shard takes (jump consistent hashing); keep the map's
  shard order and bump `version` when you change it.
- Replication and failover are PostgreSQL's own (streaming replication, for example
  run by Patroni). Point `primary` at the failover endpoint; list replicas for
  read-only actions when `read_from_replicas` is true.
- A creature's guest databases live on its shard and are reached through that shard's
  `guest_proxy`.

## RocksDB

Single host: set `ASEMAN_CORE_STORAGE_PROVIDER=rocksdb`; state lives under the storage
root. The optional signal log uses QuestDB (`ASEMAN_SIGNAL_LOG_PROVIDER=questdb`).

Cluster mode: every replica keeps a full copy; each write batch is a Raft log entry
committed on a quorum and applied in log order everywhere, and a write returns once
the writing replica has applied it. Bring a cluster up with the cluster configuration
(`ASEMAN_LEGACY_CLUSTER_*` keys — the prefix is historical — or `cluster.json` under the
storage root):

1. Start the seed replica with `bootstrap: true`; it initializes the cluster.
2. Start the other replicas with `bootstrap: false`.
3. `asemanctl cluster add-peer --id N --addr host:port` for each one (voters by
   default); `asemanctl cluster status` shows leader, terms, and replication.

All replicas must share the cluster auth token; the cluster listener refuses
administrative routes without one.

## Moving between providers

Move by export/import (A309), never by running two providers. The importers for
Caspar-era RocksDB/QuestDB data remain available for existing installations.
