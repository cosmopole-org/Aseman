---
status: DECISION
owner: storage/migration
source_of_truth: this ADR
last_verified_commit: 90d3256
verification: PostgreSQL compatibility schema unit test; optional live provider and node ITrx tests
---

# ADR 0031: PostgreSQL compatibility transactions use separated relational state

## Status

Accepted 2026-09-26 (approved by the project owner). This refines ADR 0026; it does not
change which provider is authoritative for any family.

## Context

The old `ITrx` surface combines several storage needs: object columns, secondary
indexes, relation groups, JSON documents, and unclassified operational values. Its
RocksDB implementation necessarily encodes all of them as physical keys. PostgreSQL
must be able to satisfy the same transitional behavior without depending on RocksDB or
copying that physical layout into one generic key/value table.

ADR 0026 still keeps finance, identity credentials, observed VMM state, and other
named families legacy-authoritative until their own gates pass. Merely providing a
PostgreSQL implementation is therefore not authority or deletion evidence.

## Decision

1. `aseman-storage-postgres` owns an independent compatibility transaction provider.
   The node adapter implements `ITrx` over it and has no RocksDB dependency.
2. The provider uses a private `aseman_compat` schema with separate representations:
   object columns, secondary indexes, normalized relations, JSONB documents, and an
   isolated opaque-value fallback. Structured prefixes cannot enter the fallback.
3. A relation group is stored as `relation_type`, `scope`, `member`, and value. Its
   compatibility spelling is generated and indexed; grouped access uses a covering
   relational index. Object and index dimensions are columns, document content has a
   JSONB GIN index, and prefix compatibility columns use `text_pattern_ops`.
4. One `ITrx` handle is one serializable PostgreSQL transaction. It provides
   read-your-writes, atomic commit, rollback-on-drop, bounded pooled connections, and
   parameterized SQL. High-use object and index scans query their native tables rather
   than performing a generic-key round trip per field.
5. The opaque table is a measured compatibility escape hatch, not a new application
   API. New features continue to use typed ports and capsule mappings. Each remaining
   opaque family must leave through its owning migration/removal row.
6. The adapter is initially for characterization, backfill, semantic comparison, and
   gated family cutovers. Composition may select it only after the family owner proves
   backfill and read comparison and records the authority change in RL-005 (or the
   more specific removal row).

## Migration and rollback

This implements the PostgreSQL replacement part of R10/R11 and A303/A305/A309 under
P3-06, owned by `storage/migration` and RL-005. Backfill classifies physical keys into
the five representations, compares `ITrx` semantic reads, then changes authority by
family. Rollback restores the previous provider binding generation while retaining the
PostgreSQL rows for diagnosis. No RocksDB data is removed until the existing deletion
gates pass.

## Consequences

- PostgreSQL can exercise transitional handlers without opening a RocksDB transaction.
- Link groups and common lookups use native relational indexes, while genuinely
  document-shaped values retain JSON semantics.
- The compatibility schema remains intentionally less desirable than typed ports, so
  it does not become a permanent second domain model.

## Rejected alternatives

- One PostgreSQL key/value table: simple, but preserves the RocksDB physical model and
  prevents relational constraints and efficient grouped access.
- Putting every value in JSONB: weakens typing and makes ordinary relationships and
  indexes harder to constrain and query.
- Switching every remaining family immediately: violates ADR 0026's family ownership,
  backfill, comparison, and rollback requirements.
