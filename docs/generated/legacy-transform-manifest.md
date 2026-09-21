---
status: GENERATED
owner: migration/storage
source_of_truth: docs/generated/current-storage-access.json and scripts/generate_legacy_transform_manifest.py
last_verified_commit: 800df24076c7
verification: python3 scripts/generate_legacy_transform_manifest.py --check
---

# Legacy-to-capsule transform manifest

A004 rows are exhaustively accounted for, but A308 remains `ACCEPTED`.
There are **0** blocked review rows. Unknown records fail
closed; a blocked or heuristic row is never copied as an opaque authoritative capsule.

## Direct access disposition

| Disposition | Rows |
|---|---:|
| `aggregate_with_document_family` | 3 |
| `aggregate_with_object_family` | 18 |
| `covered_by_reviewed_family` | 7 |
| `derived_index_or_relationship` | 54 |
| `fixture_backed_transform` | 76 |
| `intentional_removal` | 20 |
| `reviewed_no_persisted_record` | 7 |
| `vmm_observed_runtime` | 33 |

## Blocked rows by owning phase

| Owner | Rows |
|---|---:|

## Typed object families

| Legacy family | Target | Status |
|---|---|---|
| `Chain` | `core.chain` | `fixture_backed_transform` |
| `ChainShard` | `core.chain_shard` | `fixture_backed_transform` |
| `Creature` | `core.creature` | `fixture_backed_transform` |
| `Entity` | `core.entity` | `fixture_backed_transform` |
| `File` | `core.file` | `fixture_backed_transform` |
| `Program` | `core.program` | `fixture_backed_transform` |
| `Session` | `core.session` | `fixture_backed_transform` |
| `Store` | `core.store` | `fixture_backed_transform` |

## Reviewed JSON document families (ADR 0016)

| Legacy key | Target |
|---|---|
| `UserMeta::{id}` at `metadata` | `core.user_metadata` |
| `CreatMeta::{id}` at `metadata` | `core.creature_metadata` |
| `StoreMeta::{id}` at `metadata` | `core.store_metadata` |
| `ProgMeta::{id}` at `metadata` | `core.program_metadata` |

ADR 0017 exports the legacy finance subsystem as a reconciled, immutable
`finance.legacy_record` epoch; derived counters and listing links are verified only.

The next review must add fixture-backed transforms for the remaining VM/runtime links
and JSON, raw operational keys, Hashgraph checkpoints, and OpenRaft state-machine
authority before A308 can be accepted or any backfill can start.
