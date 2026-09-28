---
status: DECISION
owner: architecture/coordination
source_of_truth: this ADR
last_verified_commit: 800df24076c7
verification: removal ledger, dependency checks, HA chaos suite
---

# ADR 0012: Remove OpenRaft after coordination replacement

## Status

Accepted 2026-09-19. **Amended by ADR 0033 (2026-09-28):** OpenRaft is kept, but only
as the RocksDB storage provider's distribution mechanism, inside
`modules/storage/rocksdb`. It is still never a scheduler, membership authority for
anything but its own replicas, federation directory, or finance-consensus provider,
and no layer above the storage seam proposes to it.

## Decision

OpenRaft has no target production role and is removed after its current replication and
cluster behaviors are characterized and replaced. It is not retained as a scheduler,
application database, control-plane membership authority, federation directory, or
finance-consensus provider.

Aseman application state uses capsule storage/PostgreSQL guarantees. Singleton work
uses the fenced `CoordinationPort` from ADR 0013. Nomad's Raft owns Nomad
infrastructure state only. Hashgraph initially owns financial ordering/finality behind
its contract. These authorities must not elect or replicate for one another.

## Migration and rollback

Inventory every OpenRaft key/message/caller, stop new writes, export any authoritative
legacy records into their owning capsule kinds, verify replicas/checksums, and cut
cluster endpoints to the new stable control plane. Keep a read-only export through the
rollback window. Rollback reactivates the previous signed release and its complete
state checkpoint; no mixed OpenRaft/PostgreSQL writers are allowed.

Deletion requires no OpenRaft dependency, configuration, route, persisted authority,
or ambiguous `cluster` CLI behavior in the shipped Aseman path.

Rejected: retaining an undefined “optional cluster” role and using multiple consensus
systems for the same state.
