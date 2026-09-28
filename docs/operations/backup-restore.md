# Backup, restore, and resumable operations

This runbook is the A902 contract for `upgrade`, `backup`, `restore`, `doctor`, and
`support-bundle`. The ordered plans live in `aseman_domain::operations`; a driver must
persist `contracts/operations/operation-journal.schema.json` atomically after every
successful step. A completed step is never executed again on resume.

## Backup

Preflight, quiesce writes, snapshot all durable stores, capture capsule schema versions,
provider mappings and module versions, hash every artifact, sign the manifest, resume
writes, then verify the backup. Failure before `resume_writes` requires the operator to
resume writes before leaving the incident. The manifest must validate against
`backup-manifest.schema.json`; an unsigned manifest is not a backup.

## Restore

Restore is accepted only onto an explicitly selected empty target. Verify the manifest
signature and every artifact hash before changing the target. Prepare the target, restore
stores, apply the captured catalogs, verify integrity, start services, and pass health.
Never fall back to the source cluster and never overwrite a non-empty target implicitly.

## Upgrade and diagnostics

Upgrade takes a verified backup before drain and uses the same durable journal. Doctor is
read-only. Support bundle collection follows
`contracts/operations/support-bundle-redaction.json`: forbidden sources are not collected,
then structured keys and residual values are redacted before packaging. Operators must
inspect the bundle before sharing it.

## Drill acceptance

A release candidate passes only when an independently provisioned clean deployment is
restored from its signed backup, artifact hashes match, provider/module catalogs match,
health passes, and a second invocation performs no completed step. Keep the journal,
manifest, logs, and target health output as release evidence.

## Drivers

The execution drivers are `asemanctl` administration commands. Each persists its
journal under the state directory (`ASEMAN_CTL_STATE_DIR`, else
`$XDG_STATE_HOME/asemanctl`) and never repeats a completed step:

- `asemanctl doctor [--json]` — the six ordered checks: configuration, dependencies,
  storage, runtime liveness, secret permissions, and a final health gate that fails on
  any fatal finding. `--json` emits a machine-readable findings report.
- `asemanctl backup --out DIR --signing-key FILE` — preflight (target must be empty),
  quiesce writes (refused while the node is running unless `--allow-running`), snapshot
  the storage directories, capture the catalog, hash every artifact, sign the manifest
  (Ed25519 seed; also `ASEMAN_OPERATOR_SIGNING_KEY`), resume writes, and verify. When
  core storage is PostgreSQL, `snapshot/postgres/` adds the cluster role definitions
  (never passwords), a custom-format dump of the core database, one dump of every live
  creature guest database named in `aseman_core.guest_database_bindings`, and
  `databases.json` with the row count of every core table. Every dump is a consistent
  snapshot and is hashed and signed like any other artifact. The PostgreSQL client
  tools must be on `PATH` at a major version no older than either server.
- `asemanctl restore --from DIR (--trusted-key HEX | --signing-key FILE) [--force]
  [--start]` — refuse an unsigned manifest or one not signed by the trusted operator
  key, check every artifact hash before touching the target, prepare an empty target,
  restore the stores, apply the catalog, re-verify, and gate on node health after
  services start. A PostgreSQL target must be a core database without the
  `aseman_core` schema on a cluster holding none of the backup's guest databases;
  `--force` never applies to databases. Restore recreates roles, the core database,
  and each guest database with its grants, then checks every core row count.
- `asemanctl upgrade [--start]` — verify the staged binary, snapshot the current stores,
  drain (stop) the node, apply the upgrade, migrate the schema (PostgreSQL migrations
  run on node start), restart, and pass health.
- `asemanctl support-bundle [--out FILE]` — collect diagnostics, apply the checked
  redaction contract, package a tar.gz, and verify the archive. The `never_collect`
  list governs what is never read in the first place.

The rehearsed drill is `cargo test -p asemanctl --test live_backup_restore` with
`ASEMAN_TEST_POSTGRES_URL` (source) and `ASEMAN_TEST_POSTGRES_RESTORE_URL` (a separate,
empty target cluster), recorded as the A1002 `backup-clean-restore-drill` scenario.

A failed step is retried, never skipped: the journal records the failing step and a
re-run resumes there. Resumption rebuilds what earlier steps established from the
arguments and persisted files — the backup's pending manifest, the restore source
(re-verified), the upgrade install directory, the support-bundle collection — so an
interrupted process never strands its journal. The `health` gate deliberately fails
an upgrade or restore whose node has not been started, so a half-finished restart is never recorded as success.
