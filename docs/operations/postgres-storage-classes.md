---
status: CURRENT
owner: storage/postgres
source_of_truth: contracts/capsule/storage-class-semantics.json
last_verified_commit: 800df24076c7
verification: modules/storage/postgres/tests/live_postgres.rs
---

# PostgreSQL non-core storage classes

Apply `0001_core.sql` before `0002_storage_classes.sql`; cross-schema foreign keys require
the core targets. Both migrations are transactional and idempotent. Run them with a
dedicated migration role, then grant the runtime provider only the DML it needs. Public
schema access remains revoked.

Before binding a class, verify its exact capability set and semantic profile:

- telemetry is bounded, indexed observation data and never an authority source;
- audit and realtime event streams are append-only, ordered per stream, and integrity
  preserving;
- finance and outbox commits are serializable and never fall back to eventual storage;
- outbox workers claim bounded indexed batches with revision-fenced compare-and-set;
- realtime delivery is at least once, offsets advance only after processing, and global
  total order is not promised.

Backups and exports include canonical capsule bytes plus typed columns and schema version.
Restore into a disabled binding, validate counts, digests, sequence uniqueness, finance
idempotency, outbox claims, replay offsets, and retention boundaries, then switch the
binding. Rollback restores the previous binding and retains the target schemas. Never
truncate audit, acknowledged realtime, or financial tables as a rollback mechanism.
