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
