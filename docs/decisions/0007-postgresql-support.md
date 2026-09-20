---
status: DECISION
owner: storage/operations
source_of_truth: this ADR
last_verified_commit: 800df24076c7
verification: CI version matrix and extension-free conformance profile
---

# ADR 0007: PostgreSQL 17-18 baseline and optional extensions

## Status

Accepted 2026-09-19. PostgreSQL's published version policy currently lists 17 and 18
as supported; the support matrix is reviewed at every Aseman minor release.

## Decision

Aseman's first PostgreSQL provider supports majors 17 and 18 at their current minor
releases. CI tests both, production documentation recommends 18, and support for a
major ends no later than upstream end-of-life after an announced Aseman deprecation
window. Adding a newer major requires conformance, backup/restore, upgrade, collation,
query-plan, and guest-role-isolation tests; it does not silently drop the older matrix.

Core correctness uses only built-in PostgreSQL facilities. Time-series extensions
(including TimescaleDB) are optional provider capabilities and are never required for
compact or portable operation. The native default uses declarative time partitioning,
BRIN/B-tree indexes, retention jobs, and ordinary SQL. An extension-backed mapping
must round-trip canonical capsules, declare supported versions/licenses, and provide a
tested extension-free export path.

Guest creature roles cannot install extensions or receive superuser, database-create,
role-create, replication, or bypass-RLS privileges. PostgreSQL minor updates remain an
operator responsibility and are tested at the latest available patch level.

## Migration and rollback

Major upgrades use `pg_upgrade` or logical/dump migration with verified backups and
restore drills. Extension removal first exports to the native schema and verifies
semantic checksums. Rollback follows the database upgrade runbook; on-disk major
formats are never assumed backward compatible.

Rejected: supporting every upstream major, making a time-series extension mandatory,
and allowing guest-installed extensions.
