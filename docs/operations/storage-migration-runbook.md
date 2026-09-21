---
status: CURRENT
owner: storage/migration
source_of_truth: contracts/migration/protocol.md
last_verified_commit: f6be364d6761
verification: ASEMAN_TEST_POSTGRES_URL=... cargo test -p aseman-migration-e2e
---

# Legacy RocksDB to PostgreSQL migration runbook

## Prerequisites

1. Every row of `contracts/migration/legacy-transform-manifest.json` is non-blocked
   (A308 accepted).
2. Fix, or explicitly accept, the security rows in `docs/migration/legacy-defects.md`
   (LD-01 through LD-03).
3. Gather the runner evidence:
   - the currency and scale
   - the local ID origins
   - `File` byte evidence
   - `path_artifacts` for every entity artifact and resource file, each either copy
     evidence or an attested absence
   - the `node-secret-key` contents
4. Apply `0001_core.sql`, `0002_storage_classes.sql`, and `0003_migration_fence.sql`
   to a fresh database. Provision (disabled) guest databases for creatures that own
   guest KV.
5. Take a backup of the legacy storage root: application RocksDB, `cluster/raft-db`,
   the Hashgraph store, and QuestDB.

## Procedure

1. **Plan.** Create the migration with the current binding generation and a rollback
   window.
2. **Export.** Freeze legacy writes briefly and snapshot the RocksDB read-only. Run the
   reviewed transforms with the evidence, build the canonical export in dependency
   order, and record the stream digest and count.
3. **Import.** Import in bounded batches, resuming from checkpoints after a crash; a
   replay is idempotent. Import guest KV per creature binding.
4. **Verify.** Run the semantic comparison. Any divergence stops the migration: fix
   the source, then re-export.
5. **Capture the delta.** Unfreeze legacy writes with dual write enabled. Monitor
   `shadow_failures`, which must stay zero.
6. **Apply the final delta.** Freeze again, re-export, plan and apply the delta under
   the active generation, and require a clean comparison.
7. **Cut over.** Switch the binding generation, then raise the target fence to the new
   generation. The legacy side keeps receiving shadow writes.
8. **Observe the rollback window.** If rollback is needed, and only while
   `shadow_failures = 0`, run rollback, which raises the generation again. Never drop
   or truncate the target during rollback.
9. **Retire.** After the window, with operator approval, retire the legacy provider
   (RL-005 deletion gate). Do not remove VMM observed-runtime keys before RL-013
   (ADR 0022).

Also retain these for the rollback window: the OpenRaft checkpoint digests of every
replica (which must be equal), the Hashgraph block digest, and the VMM handoff
inventory.
