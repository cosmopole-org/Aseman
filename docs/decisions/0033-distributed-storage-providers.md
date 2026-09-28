---
status: DECISION
owner: architecture/storage
source_of_truth: this ADR; amends ADR 0012 and ADR 0026
last_verified_commit: pending
verification: storage conformance kit per provider and per cluster mode; live sharding, replication, and Raft suites
---

# ADR 0033: Storage providers are modules, and each distributes itself

## Status

Accepted 2026-09-28. Amends ADR 0012 (OpenRaft is kept, but only inside the RocksDB
provider) and ADR 0026 (the provider selection is total: a node never splits its
state across two providers).

## Context

Storage selection was a migration switch. `ASEMAN_CORE_STORAGE_PROVIDER=postgres`
moved the capsule families to PostgreSQL but still opened a RocksDB transaction for
every remaining family, and OpenRaft replicated RocksDB commits from inside the node's
transaction wrapper and from a deployment action. Operators need to choose a storage
system, run it on one host or as a cluster, and have the rest of the node unaware of
how that storage is distributed.

## Decision

1. **A storage provider is a module behind one seam.** A provider supplies the capsule
   unit of work (`aseman_capsule::CapsuleStore` with commit and rollback) and the
   transitional compatibility transaction surface the node's action layer uses (ADR
   0031). `ASEMAN_CORE_STORAGE_PROVIDER` selects exactly one: `postgres` (default) or
   `rocksdb`. Every node transaction runs on the selected provider only; there is no
   mixed-provider commit. Each provider passes the same storage conformance kit.

2. **Distribution belongs to the provider.** How a provider shards and replicates is
   invisible above the seam. Nothing outside a provider may call its replication
   machinery; the node, the action layer, and deployment actions see one logical
   store.

3. **RocksDB provider (`modules/storage/rocksdb`).** On one host it is embedded
   RocksDB under the storage root. In cluster mode its key/value store is replicated
   by OpenRaft: every write batch is a Raft log entry, applied in log order to each
   replica's RocksDB, and acknowledged after commit on a quorum. Membership, the Raft
   RPC listener, and the `asemanctl cluster` administration API are part of this
   provider. Today one Raft group replicates the whole key space, so every replica
   holds a full copy and any replica serves reads. Partitioning the key space into
   several Raft groups by a hash of the object identity is the provider's next
   extension; the seam does not change for it.

4. **PostgreSQL provider (`modules/storage/postgres`).** On one host it is one
   PostgreSQL database. In cluster mode (`ASEMAN_POSTGRES_SHARDS_SECRET` names a
   versioned shard map; `ASEMAN_DATABASE_URL_SECRET` names its home shard):
   - **Sharding without losing constraints.** A capsule kind is a *reference* kind
     when another kind references it or it carries a uniqueness constraint beyond its
     id; reference kinds are written to every shard (in the unit's two-phase commit)
     and read from the home shard, so every foreign key and unique index stays
     enforced by PostgreSQL on each shard. Every other kind is *distributed*: a jump
     consistent hash of the capsule id picks its shard (growing the map moves only the
     rows the new shard takes), point access touches that shard, and queries fan out
     and merge in the provider's order. The split is computed from the provider's
     table registry, so it cannot drift from the migrations.
   - **Atomicity across shards.** A unit that touched one shard commits normally. One
     that touched several prepares every shard (`PREPARE TRANSACTION
     '<decision>-<shard>'`: prepared ids are server-wide and shards may share a
     server), records the decision on the home shard, then commits every shard. On
     start, and on demand, recovery commits every prepared transaction whose decision
     is recorded and rolls back undecided ones older than a minute (presumed abort).
     Shards need `max_prepared_transactions > 0`.
   - **Replication.** Each shard names a primary and replicas. Writes use the
     primary; read-only actions may use a replica when the map sets
     `read_from_replicas`. Replication and failover are PostgreSQL's (streaming
     replication, for example operated by Patroni); the provider routes and never
     reimplements them.
   - **Placement.** A creature's guest databases are placed on a shard by the same
     hash of the creature, recorded as `provider_id = postgres-guest-v1@<shard>`, and
     reached through that shard's `guest_proxy`. Coordination, idempotency, realtime,
     federation, and the transitional compatibility schema live on the home shard.

5. **Migration.** A node moves between providers only by export/import (A309), never
   by running both. The one-way importers from legacy Caspar storage stay available.

## Consequences

- The node composes one provider and loses its OpenRaft adapter; `program` deploy
  actions no longer propose artifacts to Raft: a deployed artifact is replicated
  because the storage that holds it is.
- The PostgreSQL provider gains a sharded unit of work and a shard router; its
  single-database mode is the one-shard case of the same code.
- Cluster-mode guarantees are testable per provider: the conformance kit runs against
  a two-shard PostgreSQL cluster and a three-replica RocksDB/Raft cluster.

## Rejected alternatives

- Keeping OpenRaft as a node-level replication hook: it couples every layer to one
  provider's distribution and was the reason ADR 0012 wanted it gone.
- Reimplementing PostgreSQL replication or failover in Aseman: PostgreSQL already
  provides both, and a second failover authority would conflict with it.
- Allowing mixed providers per family: every cross-provider commit needs
  compensation logic and has no atomicity.
