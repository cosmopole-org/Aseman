---
status: CURRENT
owner: storage/postgres
source_of_truth: modules/storage-postgres/migrations and docs/decisions/0005-capsule-encoding-and-integrity.md
last_verified_commit: 800df24076c7
verification: cargo test -p aseman-storage-postgres
---

# PostgreSQL core migration and rollback

The generated `0001_core.sql` migration is additive and transactional. It creates the
private `aseman_core` schema, one native table per core kind, the trusted guest binding
catalog, typed checks, foreign keys, and indexes. It never reads, changes, or deletes
the current RocksDB/QuestDB authority.

## Apply and verify

1. Take a PostgreSQL backup and verify the target database/owner before execution.
2. Run the provider migration with an owner role; keep `PUBLIC` schema access revoked.
3. Run `ASEMAN_TEST_POSTGRES_URL=... cargo test -p aseman-storage-postgres --test live_postgres`.
4. Verify all twenty registered core tables and their constraints against
   `contracts/storage/postgres/core-mapping.json`.
5. Do not route authoritative writes until P3-05/P3-06 export, dual-write, semantic
   comparison, cutover, and rollback evidence passes.

## Rollback

Before authoritative writes, disable the provider generation and retain the schema for
inspection; the legacy binding remains authoritative, so no data restoration is needed.
After any shadow or dual writes, route back to the recorded legacy generation and keep
the PostgreSQL schema read-only for comparison. Never drop or truncate it as part of an
automated rollback. Destructive retirement is permitted only when RL-005 replacement
and deletion gates are both accepted and a reviewed backup exists.

The storage module reads its connection URI from the fixed protected secret path
`/run/secrets/aseman-postgres-core-uri`. Its signed manifest must declare the matching
secret and database egress permissions. The current local launcher correctly refuses
that non-zero permission set until an enforcing sandbox launcher is composed.
