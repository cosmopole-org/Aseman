---
status: DECISION
owner: storage/migration
source_of_truth: this ADR
last_verified_commit: f6be364d6761
verification: A304/A305 generated mapping plus A308 legacy document transforms and tests
---

# ADR 0016: Schemaless legacy document families in native capsule storage

## Status

Accepted 2026-09-20. Required by A308 before any legacy `json::` family can be
transformed; blocks the JSON portion of the P3-05 work unit and the P3-06 comparison.

## Context

Legacy Caspar stores user-supplied JSON under `json::{key}::{path}` and splats every
child path into `json::{key}::{path}.{field}` derived records. The documents have no
declared schema: the client supplies an arbitrary object and the node stores it.

A305 and A307 accepted native PostgreSQL schemas with typed columns and no JSONB entity
bucket or universal payload table, and P3-05 accepted that opaque bytes are never
promoted into authoritative target state. A schemaless document satisfies neither a
typed column set nor an opaque blob, so the transform needed a decision instead of a
guess.

## Decision

A reviewed legacy document family becomes a typed **document capsule**:

- The root record at the reviewed path is the only authority. Every `path.*` record is a
  derived projection: it is rebuilt from the root document and compared, never migrated.
- A document capsule declares a required typed relationship to its subject, a unique
  index on that subject, `document_path` (the legacy path proven by fixtures),
  `entry_count` (top-level member count), `content_digest` (domain-separated SHA-256,
  `ASEMAN-LEGACY-DOCUMENT-DIGEST-V1`, over the length-prefixed canonical encoding of the
  document value), and exactly one field of the logical type `document`.
- A `document` field holds a canonical capsule object value. It has **no native column**:
  no JSONB, no shared payload table, and no opaque byte column. Its content travels in
  the canonical envelope that every native table already stores in `capsule_cbor`, so it
  stays structured, hashable, comparable, and exportable. Physical behavior is enforced
  by the subject foreign key, the partial unique index, `entry_count`, and
  `content_digest`.
- Native filtering inside a document field is unsupported in Phase 3. A query that names
  a document field fails closed with `invalid_query`; it never degrades to a scan or to
  provider-specific JSON operators.
- The initial reviewed families are `core.user_metadata` (legacy `UserMeta::{id}`),
  `core.creature_metadata` (`CreatMeta::{id}`), `core.store_metadata`
  (`StoreMeta::{id}`), and `core.program_metadata` (`ProgMeta::{id}`), each at legacy
  path `metadata`.
- The creature type registry (`Json::CreatureType::{name}` at path `spec`) becomes the
  global `core.creature_type` document capsule, keyed by a unique `type_name` instead of
  a subject relationship. `link::CreatureTypeExists::{name} = "true"` is a derived flag
  and must exist exactly when its spec does (amended 2026-09-21).
- `UserMeta` and `CreatMeta` stay separate capsules. Legacy writes both at signup and
  then deletes them through different actions, so merging them would fabricate a
  lifecycle that the legacy store does not have.
- Every other `json::` key family remains unreviewed and fails closed until it has its
  own fixture-backed review row.

## Divergence and fail-closed rules

Legacy derived records can disagree with their root document: a merged `null` member
leaves a stale leaf, a non-merging write does not delete the previous splat, and a
member that changes from object to scalar orphans its descendants. Legacy readers can
observe those stale records through nested-path reads, so silently dropping them would
change observable behavior.

Therefore the export rebuilds every derived record from the root document and compares
it. A stored derived record that is absent from, or unequal to, the rebuild fails the
export closed and names the exact key and path for operator reconciliation. A document
whose subject object is missing from the snapshot graph — legacy delete paths leave
orphaned metadata behind — also fails closed rather than importing a dangling capsule.

## Migration and rollback

The transform is read-only and additive: it produces canonical export records only. The
legacy store stays authoritative until P3-06 comparison, cutover, and rollback gates
pass. Rollback is removal of the unused target rows; no legacy record is deleted or
rewritten by this decision.

## Rejected alternatives

- A JSONB document column: contradicts the accepted A305/A307 native-schema
  characterization and would reintroduce a payload bucket per table.
- An opaque CBOR byte column: promotes unreviewed bytes into authoritative state.
- Folding metadata into the subject entity row: fabricates a shared lifecycle and
  revision chain that legacy does not have.
- Shredding each document into per-leaf capsules: would have to invent path semantics
  for arrays, empty containers, and nulls, and cannot round-trip them exactly.
