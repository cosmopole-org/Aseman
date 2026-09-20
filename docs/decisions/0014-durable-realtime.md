---
status: DECISION
owner: realtime/operations
source_of_truth: this ADR
last_verified_commit: 800df24076c7
verification: A707/A708 conformance, restart, replay, load, and retention tests
---

# ADR 0014: PostgreSQL durable realtime as the default

## Status

Accepted 2026-09-19.

## Decision

The default compact and production provider stores the authoritative realtime event
log, subscriptions, consumer offsets, dead letters, and transactional outbox in
PostgreSQL. Event insertion and the producing application change share a transaction
where required. Workers claim bounded batches with lease/skip-locked semantics;
PostgreSQL notifications may reduce polling latency but are wake-up hints, never the
durable channel.

Events have a typed versioned envelope, tenant/authorization scope, stream and event
IDs, per-stream monotonic sequence, timestamp, producer, trace/correlation IDs, payload
digest, retention class, and optional idempotency key. Delivery is at least once;
consumers checkpoint only after processing and must deduplicate. Global total ordering
is not promised. Backpressure, quotas, replay bounds, and retention are explicit.

An in-memory provider is development/test only and advertises no durability. External
brokers may replace PostgreSQL after passing the same semantics/capacity/failure suite
and migrating offsets through capsule checkpoints.

## Migration and rollback

Wrap the legacy signaler as a non-durable provider, dual-publish from the transactional
outbox while shadowing, compare authorized recipients/order, then switch subscription
checkpoints. Rollback resumes the old provider only within its window and replays from
the durable outbox; acknowledged durable events are not discarded.

Rejected: process-local signalling in production and making an additional broker a
mandatory compact-mode dependency.
