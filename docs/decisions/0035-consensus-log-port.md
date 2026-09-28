---
status: DECISION
owner: architecture/consensus
source_of_truth: this ADR; amends ADR 0033
last_verified_commit: pending
verification: aseman-ports conformance::consensus_log against the RocksDB and PostgreSQL providers; Hashgraph store and bootstrap tests over the in-memory reference; the xtask consensus dependency rule
---

# ADR 0035: Consensus engines persist through a storage port

## Status

Accepted 2026-09-28. Amends ADR 0033: consensus logs are storage, served by the
selected provider like every other family.

## Context

The Hashgraph engine opened its own RocksDB database for each shard and depended on
the RocksDB provider crate for tuning. A node on PostgreSQL therefore still ran an
embedded RocksDB for consensus, and replacing the storage system meant editing the
consensus engine.

## Decision

1. **A consensus-log port.** `aseman_ports::consensus_log` defines
   `ConsensusLogStorage::open(name, fresh)` and `ConsensusLog` — an ordered byte
   key/value space per log with `get`, `scan_prefix`, atomic ordered `write` batches
   (put, delete, delete-range), and `flush`. `fresh` sets the current contents aside
   and starts empty. `conformance::consensus_log` is the contract, with an in-memory
   reference implementation.

2. **The engine sees only the port.** Hashgraph's `PersistentStore` (formerly
   `RocksDbStore`) keeps its in-memory cache and key layout and persists through a
   `ConsensusLog`. The engine configuration carries the `ConsensusLogStorage`; the
   log's name is the engine's data directory, as before. The engine crate depends on
   no storage provider and no database driver; `cargo xtask fast` refuses such a
   dependency for every `modules/consensus/*` package.

3. **Each provider implements it.** RocksDB: one embedded database per log, named by
   its directory, set aside by renaming to `<name>--UTC--<timestamp>`. PostgreSQL:
   rows of `aseman_consensus.log_entries (log, key, value)` on the home database,
   set aside by renaming the rows' log. Both pass the same conformance suite.

4. **The node composes it.** The node opens the selected provider's
   `ConsensusLogStorage` and hands it to the chain adapter, which gives it to every
   shard engine. Switching `ASEMAN_CORE_STORAGE_PROVIDER` switches where consensus
   logs live without touching the engine.

## Consequences

- A PostgreSQL node no longer opens RocksDB for consensus. Logs written to RocksDB
  before this change are not moved; an engine started with `bootstrap` on a node that
  switched to PostgreSQL starts from an empty log and catches up from its peers.
- A future provider (another SQL or NoSQL system) serves consensus by implementing
  one small port and passing its conformance suite.
