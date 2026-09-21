---
status: DECISION
owner: migration/storage
source_of_truth: this ADR
last_verified_commit: f6be364d6761
verification: A308 raw-key tests and generated transform manifest
---

# ADR 0020: Legacy operational raw keys

## Status

Accepted 2026-09-21. It closes the A308 semantic review of legacy raw (non-`obj`/`link`/
`index`/`json`) keys, except guest `dbOp` storage, which ADR 0021 governs.

## Decision

| Legacy raw key | Evidence | Disposition |
|---|---|---|
| `globalIdCounter`, `localIdCounter` | `drivers/storage.rs` `gen_id`; `localIdCounter` is written directly to the application RocksDB | Verified, not migrated. Each value must be exactly 8 big-endian bytes, non-negative, and at least the largest `{n}@global` (global) or `{n}@{declared local origin}` (local) object ID. New Aseman identities are UUIDv7 (ADR 0009), and legacy IDs survive only as legacy identity inputs. |
| `chainCallback::{user}_{tag}` family (`\|>{tail}`, `\|{tail}::machineId`, `::storeId`, `::attachment`, `::targetCount`, `::tempCount`) | `core/core_orchestrator.rs` writes it, and its only reader is the write-time existence check; nothing consumes or deletes it | Dead write-only state. It is shape-checked and not migrated. |
| `god::{id}` | `drivers/security.rs` reads a superuser flag that no legacy writer produces | Any record fails closed. A hand-written superuser flag requires administrator review and an explicit P4 capability grant; the migration never grants or silently drops superuser authority. |

Any other raw key remains `Unmapped` and fails the export.

## Migration and rollback

Only the typed capsules of other families are produced; these keys contribute no
target state. The legacy source is unchanged, so rollback needs nothing from Aseman.
