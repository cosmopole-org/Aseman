---
status: DECISION
owner: finance/consensus
source_of_truth: this ADR
last_verified_commit: f6be364d6761
verification: A308 Hashgraph checkpoint tests; P8-02 financial epoch switching
---

# ADR 0025: The legacy Hashgraph store is consensus-provider state

## Status

Accepted 2026-09-21. It resolves the last A308 rows (the eight Hashgraph families) without
implementing P8.

## Context

Plan 01/07 keeps Hashgraph as the default `ConsensusProvider` behind a replaceable
contract, and RL-011 moves it into `modules/consensus/hashgraph`. Its separate RocksDB
holds peers, peer sets, events, participant roots, rounds, blocks, and frames as
provider-specific marshal bytes. The application results it orders are already in the
application RocksDB, and ADR 0017 exports them as a reconciled finance epoch. Nomad's
Raft is the analogous case, and plan 04 assigns it only its own infrastructure state.

## Decision

- The Hashgraph store is private state of the consensus provider. It is not exported as
  Aseman capsules, and its bytes are not decoded in Phase 3. Decoding and cross-node
  finality verification belong to the P8 provider through its checkpoint contract.
- `LegacyHashgraphCheckpoint::read_only` opens the store without modifying it, classifies
  every key into one of the eight reviewed families (any other key fails closed),
  reports per-family counts and the highest block index, and computes a
  domain-separated digest over the sorted block family. That digest pins the finalized
  history that the ADR 0017 finance epoch was built from.
- The store stays with the Hashgraph module (RL-011) and is retained for the rollback
  window. P8-02 switches financial epochs at a finalized block that matches this
  checkpoint.
