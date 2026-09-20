---
status: ACCEPTED
owner: architecture
source_of_truth: plan/migration/14-plan-integrity-and-traceability.md
last_verified_commit: 800df24076c7
verification: contract and integration suites introduced by owning phases
---

# Authoritative data flows

## Client operation

```text
client -> network adapter -> canonical gateway DTO -> identity verification
       -> policy decision -> application use case -> typed port -> adapter/provider
       -> typed result/error -> response adapter
```

The transport cannot bypass policy or invoke a concrete driver. Request IDs,
deadlines, cancellation, actor identity, decision version, and audit context propagate
through the whole flow.

## Workload lifecycle

```text
application desired state + operation record -> VMM HTTP client
  -> aseman-vmm common validation/reconciliation -> selected backend
  -> runtime/worker agent -> observed event/usage -> VMM projection
  -> Aseman reconciliation and durable audit/outbox
```

Retries reuse the operation/idempotency identity. A VMM observation cannot authorize
or create desired state.

## Guest database operation

```text
signed workload request -> canonical verification + replay check
  -> workload -> program -> creature resolution -> policy decision
  -> trusted CreatureDatabaseBinding -> database-partitioned pool
  -> transaction-scoped dedicated role -> creature table/collection operation
  -> reset/verify session -> signed/audited response
```

The request contains no authoritative creature, database, namespace, provider, or
role. Cancellation, errors, and pool reuse execute the same reset/verification path.

## Storage migration

```text
legacy authoritative read -> canonical transform -> target write
  -> semantic checksum/read-compare -> durable migration checkpoint
  -> bounded dual write -> cutover generation -> target authoritative read
```

Rollback is permitted only before the documented irreversible point and replays the
durable journal. Provider-specific guest schemas declare portability before export.

## Federation

```text
local authorized request -> home/destination resolution -> signed envelope
  -> remote identity/replay/hop validation -> destination policy decision
  -> destination use case -> signed result/error -> local verification
```

Remote authentication never substitutes for destination authorization.

## Metering and settlement

```text
signed VMM sample -> normalize/deduplicate interval -> usage capsule
  -> effective pricing version -> reservation/settlement journal entries
  -> consensus epoch/order/finality -> balance projection + audit/outbox
```

Retries use stable sample and journal identities. Corrections append entries rather
than rewriting finalized history.
