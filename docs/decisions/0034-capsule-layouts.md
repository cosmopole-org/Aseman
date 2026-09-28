---
status: DECISION
owner: architecture/storage
source_of_truth: this ADR; amends ADR 0016 and ADR 0033
last_verified_commit: pending
verification: storage conformance kit in both layouts per provider; live PostgreSQL layout-switch and sharded suites; RocksDB three-replica capsule test
---

# ADR 0034: Flattened and capsule layouts, flattened by default

## Status

Accepted 2026-09-28. Amends ADR 0016 (a document field now has a column) and ADR 0033
(each provider offers both layouts).

## Context

Every PostgreSQL row carried the whole signed envelope in `capsule_cbor`; the typed
columns beside it were projections, and a document field existed only inside the
packed envelope. Operators asked for rows that are ordinary rows: each property of an
entity in its own column, readable and indexable by any SQL tool, with the packed
envelope available as an option rather than the rule.

## Decision

1. **Two layouts, chosen with `ASEMAN_STORAGE_CAPSULE_MODE`.** `off` (the default) is
   the **flattened** layout; `on` is **capsule mode**. Both keep the same capsule
   contract: a read returns the exact envelope that was written, verified against its
   integrity hash, and every conformance case passes in both.

2. **Flattened (default).** Every declared field is a real column, a document field is
   a JSONB column named after the field, and relationships are UUID columns. The
   envelope is rebuilt from the row on read and must verify, so a row edited outside the
   provider is refused. `capsule_shape` records the few facts columns cannot carry — a
   field present as explicit `null`, an integer held by a float column, a non-canonical
   relationship order — and is `NULL` for ordinary rows. JSONB cannot tell bytes,
   floats, or NUL-bearing text apart from other JSON, so those travel as single-key
   `$`-tagged objects (`$bytes`, `$float`, `$text`, `$entries` for an object whose own
   keys could be read as a tag). `capsule_cbor` stays `NULL`.

3. **Capsule mode.** The canonical envelope is packed into `capsule_cbor` beside the
   typed columns (the layout before this ADR); document columns stay `NULL`.

4. **The database owns its layout.** The node's migration records the configured
   layout in `aseman_core.storage_layout`; every repository and unit of work reads it
   when it connects, so tools and secondary connections never write the other layout.
   In cluster mode every shard is migrated to the same layout.

5. **The provider owns its columns.** On every migration the provider reconciles each
   mapped table with the mapping: it adds a column the mapping gained, changes a column
   whose mapped type changed (`USING column::type`), and relaxes `capsule_cbor` on
   schemas created before this ADR. It never drops a column.

6. **Switching converts in place.** Reads accept a row in either layout, so a switch
   never strands data. The migration rewrites every mutable row stored in the other
   layout; append-only rows cannot be rewritten (their tables refuse updates) and stay
   readable as they are.

7. **Key/value providers flatten too.** The RocksDB provider stores a flattened
   capsule as one key per body field (the field's canonical value) plus one metadata
   key; rewriting a capsule deletes the keys of fields it no longer has. Capsule mode
   is one key per capsule. Unique indexes from the provider-neutral logical schemas
   (`contracts/capsule/kinds`) are unique keys claimed in the same conditional batch as
   the write. In cluster mode the batch's preconditions are checked by the Raft state
   machine, in log order, on every replica, so compare-and-set and uniqueness hold
   across replicas.

## Consequences

- Default rows are plain rows: SQL tools, exports, and indexes see every property. A
  document field is still not filterable through the capsule query API (ADR 0016's
  fail-closed rule stands); its JSONB column is for operators and future indexes.
- Reads in the flattened layout rebuild and hash the envelope; that cost replaces
  decoding the packed bytes.
- The guest gateway's reserved `_aseman_legacy_kv` import table keeps its packed copy
  under its own contract (`contracts/capsule/guest/legacy-kv-table.json`); guest-defined
  tables were already plain columns.
- The RocksDB provider's capsule store advertises what it enforces: transactions,
  range queries, and unique indexes, but not foreign keys; reads are linearizable on one
  host and read-committed on a replica.

## Rejected alternatives

- Dropping the packed column entirely: capsule mode remains useful for byte-exact
  archival and for providers without typed columns.
- A per-connection layout switch: two writers in two layouts would leave a database
  that no single migration describes.
- Storing flattened documents as native JSON numbers: JSONB normalizes number text and
  cannot keep a float that happens to be integral apart from an integer.
