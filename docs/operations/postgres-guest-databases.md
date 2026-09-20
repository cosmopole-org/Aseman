---
status: CURRENT
owner: storage/postgres
source_of_truth: ADR 0001 and contracts/capsule/guest
last_verified_commit: 800df24076c7
verification: modules/storage-postgres/tests/live_guest_isolation.rs
---

# PostgreSQL guest database lifecycle

## Invariants

- A creature binding generation names one provider-created database and one `NOLOGIN`
  role. These names are never accepted from a workload request.
- Only the protected Aseman proxy login may assume a creature role. Public connect and
  public schema creation remain revoked.
- A pool is lazy, bounded, and scoped to the trusted binding generation. Every checkout
  and return resets and verifies database, session user, role, settings, and temp state.
- Guest DDL is typed and revision-fenced. Raw SQL and provider administration are not
  guest operations; the catalog and change journal are provider-private.

## Provision and stage

1. Resolve the creature from authoritative control-plane state and allocate the next
   binding generation.
2. Provision the database and restricted role in `Disabled` state under the provider
   advisory lock. Verify role flags, database privileges, schema privileges, and catalog
   revision zero.
3. Apply the portable schema commands in revision order. Import guest record capsules
   only through the migration path and verify counts, revisions, integrity, relationships,
   tombstones, and the final schema revision.
4. Run cross-creature connect, catalog, query, cursor, cache, and event isolation checks.
   Do not enable the generation if any identity or capability check is weaker than the
   source.

## Enable and observe

Enable database connect only when the signed guest gateway and trusted binding switch are
ready. Atomically publish the new binding generation, allow new pools to be created, and
observe authentication failures, revision conflicts, pool saturation, reset failures,
and role/database mismatches. A mismatch is contamination: fail the operation, discard
the authorization state, and quarantine the affected pool rather than retrying under an
unverified identity.

## Disable and roll back

1. Restore the prior trusted binding generation so new requests cannot reach the target.
2. Mark the target draining, revoke connect, terminate remaining target sessions, and
   remove its pool entry.
3. Mark it disabled/retained and preserve its database, role, schema journal, and export
   checkpoint for diagnosis and forward reconciliation.
4. Re-run semantic comparison before any later re-enable. Never drop a database or role
   during ordinary rollback.

## Retirement

Physical deletion requires the RL-006 replacement and deletion gates, an expired rollback
window, a verified portable export, and explicit retirement approval. Terminate exact
database sessions, drop the exact retained database, revoke membership, then drop the
exact dedicated role. Broad patterns, workload-provided identifiers, and recursive
cleanup commands are prohibited.
