---
status: DECISION
owner: storage/application
source_of_truth: this ADR
last_verified_commit: 800df24076c7
verification: A303 registry plus storage-provider conformance suite
---

# ADR 0006: Consistency profiles by capsule kind

## Status

Accepted 2026-09-19.

## Decision

Every capsule kind declares one of these minimum profiles; a provider may be stronger:

| Profile | Required semantics | Initial kinds/classes |
|---|---|---|
| `serializable` | Serializable transactions, compare-and-swap revision, durable commit, unique constraints. | Identities, creatures, programs, policy/grants/revocation, workload desired state/operations, module registry, guest bindings/schemas, coordination, wallets, pricing, ledger, settlement, outbox publication claim. |
| `snapshot` | Repeatable/snapshot transaction plus revision conflict detection. | General core repositories and guest schema/data transactions requesting multi-record atomicity. |
| `append_linearizable` | Per-stream linearizable append, stable idempotency identity, immutable records, ordered checkpoint. | Audit and finalized usage/consensus records. |
| `read_committed` | Durable atomic record mutation and read-your-writes in a session. | Guest data explicitly opting out of multi-record snapshot semantics and non-authoritative operational records. |
| `eventual` | Versioned last-writer/merge rule, bounded-staleness disclosure, no authority decisions. | Telemetry aggregates, disposable caches, remote directory caches, non-authoritative observations. |

Financial, authorization, identity, idempotency, binding, schema, and outbox state may
never fall back to eventual consistency. Cross-kind transactions declare their atomic
set; otherwise a durable saga/outbox coordinates them. Activation rejects missing or
ambiguous guarantees.

## Migration and rollback

A303 assigns every kind and records any stronger current guarantee. Migration verifies
conflicts, uniqueness, and restart durability before cutover. Rollback is forbidden if
the old provider cannot represent commits made under the declared profile without an
explicit compensating/export protocol.

Rejected: one global consistency level and provider-chosen silent weakening.
