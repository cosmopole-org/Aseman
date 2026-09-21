---
status: DECISION
owner: vmm/migration
source_of_truth: this ADR
last_verified_commit: f6be364d6761
verification: A308 VM-state transforms and tests; P5 reconciliation conformance (A502/A503)
---

# ADR 0022: Legacy VM state — observed runtime is rebuilt, durable intent migrates

## Status

Accepted 2026-09-21. It resolves the ordering conflict between A308 (Phase 3) and the VM
state model (A501–A503, Phase 5). The chosen option: legacy observed VM runtime state is
not exported as Aseman capsules; it is rebuilt by P5 restart reconciliation.

## Context

Plan 04 states: "Aseman owns desired workload state, identity, policy, and finance. The
VMM owns observed runtime state." It also requires extracting the embedded VMM into
`modules/vmm-backend/native-legacy` with valid runtime behavior preserved (RL-013).

Legacy evidence:

- `/programs/runEntity` records an instance through `VmInstance::{program}::{entity}::{vm}`,
  `VmStatus::{vm} = running`, `VmStartedAt::{vm}`, and runtime plan links (Docker
  `VmContainerName`). `/programs/stopEntity` deletes them. Launch `params` and
  `resources` are never persisted, except inside the billing payment record.
- `install_program_bootstrap` never relaunches instances after a node restart. It only
  re-registers programs with the VMM and replays pending alarms. Instance records are
  therefore observations that can go stale; they are not a restorable desired state.
- Deployments, gateway routes, alarms, VM resource stores and entities, and proxy entity
  configuration are durable, caller-created intent. They survive restarts, and legacy
  behavior depends on them.

## Decision

### Observed runtime: owned by the VMM, not exported, rebuilt by reconciliation

| Family | Notes |
|---|---|
| `VmInstance`, `VmStatus`, `VmStartedAt`, `VmOwnerProgram`, `vmDistributed` | instance observations |
| `VmContainerName`, `vmStandaloneImageName`, `vmStandaloneContainerName` | runtime plan links (closed set; only Docker returns plan links) |
| `VmTerminal`, `VmBuilds` | session and build observations; build logs are QuestDB telemetry |
| `Json::ProxyCorrelation`, `ProxyCorrExpiry` | in-flight proxy correlations with expiry |
| `ModalApp`, `ModalImage`, `ModalVolume`, `ModalSandbox`, `ModalProvisioning`, `ModalProvisioningError` | handles to external Modal resources |

- The export shape-checks these records, so an unknown family still fails closed. It
  emits no capsule and reports per-family counts as the **VMM handoff inventory**.
- They stay in the legacy RocksDB as the native-legacy backend's runtime state until
  RL-013 extraction. RL-005 cutover and deletion must not remove them before RL-013
  passes its replacement and deletion gates.
- In P5, the native-legacy backend reports these instances as observed workloads, and
  reconciliation creates or updates `core.workload` records (desired and observed
  generations under A503). Phase 3 never fabricates a desired `core.workload`.
- `ModalVolume` handles point at external user data. They must be adopted by the P6
  runtime module or explicitly released by an operator; neither the migration nor
  deletion may orphan them.

### Durable intent: migrated as Aseman capsules

| Legacy source | Target |
|---|---|
| `vmHttpRoute::{creature}::{path}` (target JSON) | `core.gateway_route`; the reverse `vmHttpRouteFor` link and `vmHttpRouteUser` alias are derived and verified; a pinned VM instance ID is observed state and is not carried |
| `vmAlarmStoreId`, `vmAlarmTime`, `vmAlarmData`, `vmAlarmEntity` (per program) | `core.program_alarm` (the entity defaults to `main`, as the legacy replay does) |
| `Json::VmResourceStore::{store}` (`core`, `metadata`) plus `vmOwnedStore::{machine}::{store}` | `core.vm_resource_store` document capsule owned by the machine creature |
| `Json::VmResourceEntity::{store}::{type}::{id}` (`payload`, `meta`) plus its `data` file | `core.vm_resource_entity` document capsule with file-copy evidence |
| `Json::ProxyEntity::{program}::{entity}` (`config`) | `core.entity_config` document capsule |
| `vmEntityPath`, `vmEntityDownloadable` (artifact paths) | `core.entity_artifact` with file-copy evidence; `vmEntityType` must equal the entity's normalized type |

### Removed or derived

- `vmDistribution::{program}[::{entity}] = cluster|local` is OpenRaft replication scope and
  is removed under ADR 0012; placement belongs to the P6 scheduler.
- `VmBilling::{vm} = "true"` is the billing sweep's listing flag. It must match a
  `Json::VmBilling` payment record exactly (ADR 0017).

## Consequences and legacy defects

- Deleting a resource store or entity uses unprefixed raw keys and never removes the
  documents. The documents are what legacy `get` still returns, so they migrate.
- Stale `VmStatus = running` records keep being billed by the legacy sweep. P8
  reconciliation must settle the finance epoch against observed runtime, not against
  these records.
- Neither artifacts nor resource-entity files are emitted without the same bounded
  copy evidence that legacy `File` requires; a missing or unproven file fails closed.

## Rejected alternatives

- Exporting instance records as desired workloads: legacy never treated them as
  restorable intent, and launch parameters are missing.
- Pulling A501–A503 into Phase 3: the chosen resolution keeps the plan's phase order.
- Dropping Modal handles: this orphans external user data.
