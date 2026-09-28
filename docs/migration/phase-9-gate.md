---
status: PARTIAL
owner: migration/phase-9
source_of_truth: plan/migration/09-migration-phases.md (Phase 9)
last_verified_commit: eebb9c5
verification: cargo test -p aseman-domain bootstrap; cargo xtask fast
---

# Phase 9 exit gate

## Decision

**Not accepted.** The first half of the criterion is now observed: from empty state,
`asemanctl bootstrap --profile compact` brought all five services (PostgreSQL 18, VMM,
Nomad backend, node, meter) to healthy in 15 seconds on a development host against an
operator-run Nomad, and a re-run is a no-op. The gate stays partial because the rest of
its criterion — a production topology adding/draining workers and restoring from backup
— and the signed-release inputs are not yet retained as evidence.

## Compact bootstrap observation (2026-09-28)

The first real end-to-end run found eight defects that static checks had passed; each
is fixed and, where a check can hold it, guarded:

| Defect | Fix and guard |
|---|---|
| PostgreSQL 18 images refuse a volume at `/var/lib/postgresql/data` | mount `/var/lib/postgresql` |
| Services run as uid 65532 but secrets were `0600` operator-owned | `hand_to_runtime`: private files `0400` owned by their consumer (65532, or the postgres image's own user), certificates world-readable |
| The roles init script failed unreadable and the schema stage still passed | the stage verifies `aseman_guest_proxy` exists |
| Health probed a fixed `127.0.0.1:8080`, which another service may own | `--public-port`/`--health-port`, persisted in `compact.env`; preflight refuses a bound port |
| `debian:12-slim` (glibc 2.36) cannot run binaries from the `ubuntu-24.04` builders | all images on `debian:13-slim` |
| The node parsed `ASEMAN_NODE_PRIVATE_KEY_SECRET` as inline PEM, not the secret file the contract names | read the file; inline PEM stays for the `OWNER_PRIVATE_KEY` alias |
| PostgreSQL had an off-host route while the VMM/Nomad-backend namespace had none; the node's published ports were on an internal-only network, so Docker published nothing | PostgreSQL `control` only; VMM `control`+`scheduler`; node `control`+`public` — held by `check_deploy_topology.py` |
| The node required QuestDB and a pre-provisioned Babble validator, neither of which the profile supplies | `ASEMAN_SIGNAL_LOG_PROVIDER=postgres` (both profiles); bootstrap generates the validator key with the image's `aseman-keygen` and a single-peer genesis |

Observed: every service healthy and non-root (uid 65532 for Aseman images), the node
heading its own Hashgraph chain, `{"status":"ok"}` on the host-local health port, and
the A701 listener answering over TLS with an RFC 9457 `401` for an unauthenticated
contract route. The images were built locally from this commit with
`--allow-unsigned-local`; a run from signed, digest-pinned release images remains.

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
- **Deployment observation.** The compact run is observed (above) from locally built,
  unsigned images. A run from signed release images, and a production clustered run
  with worker add/drain, have not yet been retained. The stable cluster load balancer,
  HA PostgreSQL, and Nomad quorum are operator-owned inputs.
- **Restore of a whole deployment.** `backup`/`restore`/`upgrade` now carry
  PostgreSQL core storage (roles, core database, every creature guest database) in
  the signed, hashed snapshot, and the `live_backup_restore` drill restores onto an
  independently started empty cluster. Restoring a complete compact deployment and
  passing its health gate remains.
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
