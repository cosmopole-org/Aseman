---
status: CURRENT
owner: architecture
source_of_truth: plan/migration/14-plan-integrity-and-traceability.md
last_verified_commit: 800df24076c7
verification: terminology drift check plus manual review
---

# Aseman glossary and legacy-name map

This glossary is normative for new contracts and documentation. “Current” describes
the checked-in Caspar implementation; “target” describes the accepted Aseman
architecture. A legacy name is not evidence that its behavior should be preserved.

## Canonical terms

| Term | Meaning |
|---|---|
| Aseman node | One externally visible federated control-plane identity. It may be implemented by multiple internal replicas. |
| Control plane | Aseman API/application services and their coordination facilities. Nomad servers are infrastructure used by a VMM provider, not Aseman's application authority. |
| Worker | A host running a scheduler client and, where required, the restricted `aseman-vmm-agent`. |
| Workload | A managed executable instance: container, microVM, WASM instance, JavaScript runtime, or another declared runtime. “VM” is a compatibility synonym only when the operation applies to every workload. |
| Creature | The ownership, authorization, and guest-data sharing boundary for programs and their workloads. |
| Program | A deployable workload definition owned by one creature. |
| Guest | Code executing inside a workload and calling an Aseman guest API. It is not trusted merely because it runs on an Aseman worker. |
| Port | A narrow in-process behavioral interface required by application logic. |
| Adapter | In-process code translating a port or wire/domain representation. |
| Module | A signed, independently installed process or OCI artifact with a declared contract and permissions. |
| Provider | A module implementing one replaceable platform capability. |
| Runtime driver | VMM-side code that controls one workload technology. |
| Capsule | A portable logical persistence envelope and its revision/integrity semantics, independent of physical row/document/key layout. |
| Guest database | A provider-native database or equivalent namespace dedicated to one creature. It may contain multiple creature-managed tables or collections. |
| Guest role | A provider-native restricted principal dedicated to one creature and unable to access another creature's namespace. Workloads never receive its credentials. |
| Guest-data proxy | The Aseman service that verifies signed workload requests, resolves the trusted creature binding, authorizes the action, and performs it under the bound role. |
| Federation | Discovery and authorized operations between independently administered Aseman nodes. It is distinct from scheduler datacenter federation. |
| Realtime | Authorized event delivery with explicit ordering, durability, replay, and retention capabilities. |
| Coordination lease | An expiring lease carrying a monotonically increasing fencing token; liveness without fencing is not leadership. |
| Desired state | Aseman-owned declaration of the workload or module state that should exist. |
| Observed state | VMM/provider-owned report of what currently exists. It is never silently promoted to desired state. |

## Caspar-to-Aseman mapping

| Current/legacy term | Canonical target | Compatibility rule |
|---|---|---|
| Caspar / `caspar-node` | Aseman / `aseman-node` | Deprecated alias during the compatibility window; never use in new public contracts. |
| `casparctl` | `asemanctl` | Command shim must emit an actionable deprecation warning and preserve catalogued exit behavior until expiry. |
| `CASPAR_*` environment variables | `ASEMAN_*` typed configuration | Legacy aliases are read only by the compatibility layer; conflicting old/new values fail closed. |
| `caspar_client` / legacy client SDK | Generated Aseman clients | Retain only catalogued wire compatibility, not implementation coupling. |
| VM | Workload | Keep “VM” in legacy protocol fields and technology-specific operations only. |
| VM manager / embedded VMM | Aseman VMM service and selected backend provider | Desired/observed state and operation semantics cross the versioned VMM HTTP boundary. |
| VM plugin | Runtime module or driver | Compile-time aggregation is legacy; target modules are signed and independently installed. |
| master | Control-plane replica or scheduler server | Use the precise owner. “Master” is allowed only when quoting a legacy interface. |
| node mesh / OpenRaft cluster | Aseman control-plane coordination or VMM worker topology | Do not treat consensus, scheduling, and application coordination as interchangeable. |
| shell action | Gateway command/use case | Transport framing is an adapter; authorization and orchestration belong to application use cases. |
| database host call | Signed guest-data proxy operation | Caller-supplied namespace, database, and role selection are forbidden. |
| RocksDB/QuestDB records | Legacy storage-provider representations | Logical meaning migrates to capsules; physical keys/tables are not the target contract. |
| signaler | Realtime provider | Process-local delivery is non-durable and not production authority. |
| hashgraph core | Hashgraph consensus provider | Ordering/finality only; pricing and ledger rules remain Aseman application concerns. |

## Naming rules

1. New package, binary, environment, API, schema, and documentation names use
   `Aseman`/`aseman`/`ASEMAN`.
2. Compatibility aliases live at an edge and link to a removal-ledger entry.
3. A type name states its authority: for example `DesiredWorkload`,
   `ObservedAllocation`, and `CreatureDatabaseBinding` are not interchangeable IDs.
4. “Database” and “namespace” refer to provider-native isolation. “Capsule” refers to
   portable logical data, not a required shared physical table.
5. “Supported” means the operation appears in the support manifest and has executable
   characterization evidence; code presence alone is insufficient.
