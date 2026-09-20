---
status: ACCEPTED
owner: architecture
source_of_truth: plan/migration/01-target-architecture.md and plan/migration/14-plan-integrity-and-traceability.md
last_verified_commit: 800df24076c7
verification: architecture tests introduced in Phase 1
---

# State authority model

Every mutable fact has one authority. Replicas, caches, projections, provider jobs,
bootstrap snapshots, and compatibility stores may be rebuilt and may not silently
become authoritative.

| State | Authority | Derived/non-authoritative copies | Required conflict behavior |
|---|---|---|---|
| Users, creatures, programs, policies, grants | Aseman application through capsule storage | Gateway caches, provider labels | Reject stale revision; audit the conflict. |
| Desired workload state and durable operations | Aseman application through capsule storage | VMM requests/jobs | Generation and idempotency key decide; reconcile. |
| Observed allocation/runtime state | Active VMM endpoint/backend | Aseman's observation projection | Backend observation wins for facts; it cannot rewrite intent. |
| Physical database representation and provider roles | Selected storage provider | Capsule exports and migration manifests | Provider binding generation and cutover state decide. |
| Logical schemas, capsule kinds, guest bindings | Aseman application through capsule storage | Provider catalogs, pool entries | Trusted binding only; ambiguity denies access. |
| Creature guest records | Creature's dedicated provider database/namespace under its role | Export/checksum snapshots | Provider transaction semantics apply; cross-creature access always denies. |
| Workload placement | Selected VMM provider | Aseman desired-state hints | VMM placement is observed and reconciled. |
| Federation home record | Workload's home Aseman node | Remote directory caches | Signed epoch/revision and expiry decide. |
| Wallet journal | Finance ledger provider under Aseman invariants | Balance projections | Append-only corrective entry; never edit history. |
| Financial order/finality | Active consensus provider for the recorded epoch | Pending projections | Epoch contract decides; no cross-provider implicit finality. |
| Module desired state | Signed module-registry capsules | Local bootstrap snapshot | Snapshot can start recovery but cannot overwrite newer authority. |
| Realtime offsets | Active durable realtime provider plus capsule checkpoints | Subscriber memory | Resume from committed checkpoint; duplicates are idempotent. |
| Singleton leadership | Active `CoordinationPort` lease and fencing token | Process liveness | Every committed effect rejects an expired/older fence. |
| Node federation identity | Aseman identity record/key epoch | Replica certificates/endpoints | Replica loss cannot create or change node identity. |

## Rules

- Cross-authority workflows use durable operation records, outboxes, idempotent
  commands, deadlines, and reconciliation rather than distributed transactions.
- A compatibility path may mirror state only with an explicit source, generation,
  comparison rule, rollback point, and removal-ledger entry.
- When two stores claim authority without an accepted cutover record, fail closed and
  surface an operator-visible conflict.
- The guest-data proxy derives database and role from the authenticated workload,
  program, and creature binding. Request fields and connection-pool residue have no
  authority.
