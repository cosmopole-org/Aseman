# A309 storage provider migration protocol

Status: ACCEPTED. Source: plan 03 "Provider migration", ADRs 0005, 0006, and 0016–0025.
Implementation: `aseman-domain::storage_migration` (state machine and comparison),
`aseman-application::storage_migration` (service and dual-write router),
`aseman-contracts::migration` (canonical stream, semantic digest, and delta planning),
`aseman-storage-postgres::{migration, guest}` (fenced writer, snapshot, and guest
legacy KV importer), and `tests/migration` (live end-to-end proof).

## States

`Planned → Exported → Imported → Verified → CapturingDelta → DeltaApplied → CutOver →
Retired`. `Aborted` is reachable before delta capture. `RolledBack` is reachable from
`CapturingDelta`, `DeltaApplied`, and `CutOver`.

| Step (plan 03) | Transition | Guard |
|---|---|---|
| 1 validate | `plan` | Target capabilities are compatible (A303); the rollback window is positive |
| 3 export | `record_export` | Canonical stream digest and record count recorded |
| 4 import | `record_import` | The import digest and count equal the export |
| 5 verify | `record_verification` | Semantic comparison is clean: counts per kind, identities, semantic digests, and no dangling references |
| 6 dual write | `start_delta_capture` | The source is authoritative; every write is shadowed to the target |
| 7 final delta | `record_delta` | Comparison is clean **and** no shadow write failed |
| 8 switch | `cutover` | The binding generation moves to the target, which becomes authoritative; the source keeps receiving shadow writes |
| 9 window | `rollback` | Allowed only while no post-cutover shadow write to the source failed (ADR 0006); the generation increases again |
| 10 retire | `retire` | The window has elapsed **and** explicit operator approval is given |

## Invariants

- The comparison uses the **semantic digest** (kind, ID, class, owner, tombstone,
  relationships, and body), never physical bytes or the revision chain (ADR 0005).
- Delta writes preserve revision chains: a replacement is `revision + 1`, linked to
  the prior integrity hash with the creation time kept, and a removed record becomes a
  tombstone revision.
- Every routed write carries the binding generation it was routed under. Providers
  reject a generation below their fenced minimum in the same transaction as the write,
  and the fence only increases.
- A failed shadow write never fails the caller, but it is recorded durably: it blocks
  the delta before cutover and forbids rollback after it.
- Guest pairs import only into the database of the creature named by the capsule
  owner; the export never carries database, role, or generation names (ADR 0001/0021).
- State transitions are compare-and-swap on the stored phase.
