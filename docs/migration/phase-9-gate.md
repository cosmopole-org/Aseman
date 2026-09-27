---
status: PARTIAL
owner: migration/phase-9
source_of_truth: plan/migration/09-migration-phases.md (Phase 9)
last_verified_commit: eebb9c5
verification: cargo test -p aseman-domain bootstrap; cargo xtask fast
---

# Phase 9 exit gate

## Decision

**Not accepted.** The bootstrap driver, packaging policy, deployment profiles, and
operational assets are delivered and proven statically; the required clean-host outcome
has not been retained. This gate is partial because its criterion is observable — "a
clean host reaches healthy compact mode through one workflow" — not because another
repository implementation is standing in for that observation.

## What is delivered

| Item | Evidence |
|---|---|
| An idempotent, resumable, safely-rollbackable bootstrap workflow | `aseman-domain::bootstrap` with seven cases (P9-01) |
| Canonical compact bootstrap driver | `asemanctl bootstrap`; atomic progress, generated off-tree secrets/PKI, digest-pinned images, ordered Compose startup, and health gate (P9-01) |
| Executable compact profile | `deploy/compose/compact.compose.yaml`; separate hardened services, A504 sharing only the VMM network namespace, operator-supplied Nomad (A901) |
| Executable clustered service profile | `deploy/compose/cluster.compose.yaml`; three replicas sharing one identity, external PostgreSQL/Nomad, host-local upstreams for the operator load balancer (A901) |
| Generated configuration reference | `docs/generated/current-configuration.{json,md}`, in the gate |
| Generated CLI inventory | `docs/generated/current-cli-ops.{json,md}`, in the gate |
| Generated public API reference | `docs/generated/public-api.md` (A701), in the gate |
| Deployment profiles, ports, ACLs, and certificates | `contracts/deploy/topology.json` and `docs/operations/topology.md` (A602), checked in the gate |
| Operational runbooks | storage migration, VMM handoff, topology, stateful moves |
| No multi-process container assumptions | Each service is one process with one listener (A602); the agent is the only privileged one (A603) |
| Canonical node and CLI executable roots | `apps/aseman-node`, `apps/asemanctl`; the old binaries are warning compatibility edges (P9-05, RL-001/RL-015) |
| Independent metering executable | `apps/aseman-meter`; typed configuration, mutual-TLS A501 collection, PostgreSQL samples/intervals/pricing/ledger, and idempotent settlement (P9-05) |
| Separate least-privilege image definitions | `deploy/images/{node,vmm,meter,nomad-backend}.Dockerfile`; non-root, one process, no Docker/KVM access, and checked against `build-dist.sh` by `check_deploy_topology.py` |
| Restricted host-agent profile | `apps/aseman-vmm-agent` serves A603 on loopback with mTLS plus signed grants; `deploy/systemd/aseman-vmm-agent.service` limits devices, capabilities, and writable paths |
| Resumable administrative recovery contracts | `aseman-domain::operations`, signed backup-manifest and journal schemas, and `docs/operations/backup-restore.md` (A902) |
| Support-bundle secrecy boundary | deny-before-collect contract plus recursively tested structured/value redaction in `asemanctl` (A904) |
| Canonical documentation/operations ownership roots | Root architecture/contribution/security/changelog files, `docs/README.md`, `deploy/`, and the checked `tests/evals/agent/cases.json` catalog (P9-05) |

## What is not

- **Compatibility expiry for `asemanctl`.** `apps/asemanctl` now owns the
  implementation and `casparctl` is a one-way warning shim. The
  `doctor`/`backup`/`restore`/`upgrade`/`support-bundle` execution drivers are
  delivered (A902) and drive the ordered resumable journals over files, directories,
  checks, and processes. A versioned contract now gate-checks the complete dispatch
  set, exact exit-code meanings, output modes, and reserved JSON error envelope.
  Every command now supports one redacted `--json` result/error envelope, and generated
  TypeScript/Python public clients cover all 76 A701 operations. Only compatibility-
  usage/expiry observation remains open (RL-015).
- **Deployment observation.** The compact and clustered profiles and resumable compact
  driver now exist and pass static/configuration and identity-generation tests, but a
  clean-host compact run and a production clustered run have not yet been retained as
  gate evidence. The stable cluster load balancer, HA PostgreSQL, and Nomad quorum are
  operator-owned inputs.
- **Upgrade, backup, restore, doctor, and support bundle drivers operating
  databases.** The file/directory/process drivers run; database-level backup for
  PostgreSQL core storage goes through the A309 capsule export, schema migration runs
  on node start, and a clean-host restore drill has not yet been executed.
- **Moving tracked `dist/*` blobs out of source control** (RL-018). Deleting them
  before the new out-of-tree release workflow has a retained successful tagged run,
  independent checksum/attestation verification, scanner evidence, promoted artifacts,
  and migrated consumers would break installation. They stay until that replacement
  evidence exists. That is a deliberate refusal to do half of a two-part gate.
- **Observability deployment observation.** The checked A905 dashboard, alerts, SLOs,
  error-budget policy, and capacity worksheet are delivered; production metric
  emission, routing, and retained baseline/burn evidence remain external observations.

## Why this is recorded rather than glossed

The migration's own rule is a two-part gate: prove the replacement, then delete the
superseded path. Publishing artifacts and building images need infrastructure outside
this repository. Recording the gate as partial, with the list above, is what lets the
next person see exactly what remains — and keeps `check_removal_ledger_due.py` honest,
since an accepted Phase 9 would immediately make RL-015 and RL-018 overdue.
