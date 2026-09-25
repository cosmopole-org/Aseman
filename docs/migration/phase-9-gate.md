---
status: PARTIAL
owner: migration/phase-9
source_of_truth: plan/migration/09-migration-phases.md (Phase 9)
last_verified_commit: eebb9c5
verification: cargo test -p aseman-domain bootstrap; cargo xtask fast
---

# Phase 9 exit gate

## Decision

**Not accepted.** The rules a bootstrap must obey are delivered and proven; the
packaging that would let a clean host run one command is not. This gate is recorded as
partial rather than claimed, because its criterion is an observable outcome — "a clean
host reaches healthy compact mode through one workflow" — and that has not been
observed.

## What is delivered

| Item | Evidence |
|---|---|
| An idempotent, resumable, safely-rollbackable bootstrap workflow | `aseman-domain::bootstrap` with seven cases (P9-01) |
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
| Canonical documentation/operations ownership roots | Root architecture/contribution/security/changelog files, `docs/README.md`, `deploy/`, `examples/`, and the checked `evals/agent/cases.json` catalog (P9-05) |

## What is not

- **Completing `asemanctl` administration groups.** `apps/asemanctl` now owns the
  implementation and `casparctl` is a one-way warning shim, but the required
  backup/restore/policy/finance execution surfaces are incomplete (RL-015).
- **An executable compact/cluster profile and one-command bootstrap** on a clean host;
  image definitions exist, but deployment health has not been observed.
- **Upgrade, backup, restore, doctor, and support bundle execution drivers.** Their
  ordered resumable journals, backup manifest, runbook, and support-bundle redaction
  are implemented; the commands do not yet operate databases, files, and services.
- **Moving tracked `dist/*` blobs out of source control** (RL-018). Deleting them
  without a signed-artifact pipeline to replace them would break installation, so they
  stay until there is one. That is a deliberate refusal to do half of a two-part gate.
- **Observability dashboards.**

## Why this is recorded rather than glossed

The migration's own rule is a two-part gate: prove the replacement, then delete the
superseded path. Publishing artifacts and building images need infrastructure outside
this repository. Recording the gate as partial, with the list above, is what lets the
next person see exactly what remains — and keeps `check_removal_ledger_due.py` honest,
since an accepted Phase 9 would immediately make RL-015 and RL-018 overdue.
