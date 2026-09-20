---
status: DECISION
owner: architecture/operations
source_of_truth: this ADR
last_verified_commit: 800df24076c7
verification: A607 coordination conformance and HA chaos suite
---

# ADR 0013: Replicated stateless control plane with PostgreSQL fenced leases

## Status

Accepted 2026-09-19. This is required artifact A014.

## Decision

Compact mode runs one Aseman control replica. The production default runs three
stateless Aseman API/control replicas across failure domains behind one stable HTTPS
endpoint; operators may run more odd or even API replicas because PostgreSQL—not an
Aseman replica quorum—owns application state and coordination. Nomad server quorum is
separate and normally three or five servers.

One node ID and node signing-key lineage are stored in authoritative capsule state.
Replicas receive scoped service identities and may sign as the node only through the
node key service/policy. Replica membership, address, or loss never changes the node ID
seen by federation.

Singleton reconciliation, outbox publication, directory publication, migrations, and
scheduled settlement acquire a named `CoordinationPort` lease. The PostgreSQL provider
uses a transactionally locked lease row, database time, owner instance ID, expiry, and
a monotonically increasing 64-bit fencing token allocated on every acquisition. Renew
and release compare owner and token. Every committed singleton effect records/checks
the token in the same authoritative transaction or through a destination-side
last-token guard. An advisory lock alone is insufficient.

Loss of database connectivity or renewal makes the holder stop before expiry plus the
declared skew/safety margin. A paused former holder cannot commit with an old token.
Readiness distinguishes API service from dependencies and eligibility for singleton
work. Stable endpoints use an operator/load-balancer address with health-based routing;
replica addresses are not published as node identity.

## Migration and rollback

Introduce the coordination port and fence every singleton effect while one legacy
replica is active. Then add replicas, exercise kill/pause/partition/clock-skew cases,
and finally remove process-local/OpenRaft leadership. Rollback scales to one replica
while retaining the PostgreSQL lease/fence records; it never runs old and new leaders
against the same unfenced effect.

Rejected: process liveness as leadership, advisory locks without effect fencing,
OpenRaft control-plane authority, and federation identity per replica.
