---
status: GENERATED
owner: migration/storage
source_of_truth: docs/generated/current-storage-access.json and scripts/generate_legacy_transform_manifest.py
last_verified_commit: 800df24076c7
verification: python3 scripts/generate_legacy_transform_manifest.py --check
---

# Legacy-to-capsule transform manifest

A004 rows are exhaustively accounted for, but A308 remains `IN_PROGRESS`.
There are **372** blocked review rows. Unknown records fail
closed; a blocked or heuristic row is never copied as an opaque authoritative capsule.

## Direct access disposition

| Disposition | Rows |
|---|---:|
| `aggregate_with_object_family` | 36 |
| `blocked_payload_fixture` | 42 |
| `blocked_semantic_review` | 15 |
| `derived_index_or_relationship` | 150 |

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

The next review must add fixture-backed graph transforms for owners, relationships,
JSON payloads, QuestDB signal history, Hashgraph checkpoints, and OpenRaft state-machine
authority before A308 can be accepted or any backfill can start.
