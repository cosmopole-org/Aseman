---
status: DECISION
owner: vmm/storage
source_of_truth: this ADR
last_verified_commit: 800df24076c7
verification: A605 contract, compatibility reports, migration/rollback tests
---

# ADR 0011: Declared stateful portability tiers; no implicit live migration

## Status

Accepted 2026-09-19.

## Decision

Every workload volume and snapshot declares one portability tier:

- `ephemeral`: recreation may discard data; never presented as migrated.
- `provider_local`: movable only by a provider-declared snapshot/export contract.
- `portable_offline`: quiesce, snapshot, checksum, copy, restore, verify, then start.
- `shared_external`: data remains in a separately managed storage provider and the
  target must pass attach/fencing checks.

Live stateful migration is not an initial Aseman guarantee. An operation crossing
worker, runtime, or VMM-provider boundaries is rejected unless every attached volume,
runtime state, CPU/architecture requirement, and snapshot format has a compatible
declared path. Crash-consistent and application-consistent snapshots are distinct.

Portable-offline moves use a durable journal, source fencing, generation/checksum,
quiesce timeout, encrypted transfer, target restore verification, and an explicit
irreversible point. Source and target may not be writable simultaneously.

## Migration and rollback

Legacy volumes begin as `provider_local` until proven otherwise. Rollback before
cutover unfences and resumes the source. After verified target write activation,
rollback is a new reverse migration; it is never an unfenced source restart.

Rejected: claiming universal snapshot portability, best-effort copying while running,
and silently recreating stateful workloads as ephemeral.
