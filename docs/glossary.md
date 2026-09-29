---
status: CURRENT
owner: architecture
source_of_truth: this glossary
verification: manual review
---

# Aseman glossary

This glossary is normative for contracts, code, and documentation. Aseman was
Caspar; the last table maps the Caspar names that still appear in history, imported
data, and the retired-name configuration catalog.

## Canonical terms

| Term | Meaning |
|---|---|
| Aseman node | One externally visible federated control-plane identity. It may be implemented by multiple internal replicas. |
| Control plane | Aseman API/application services and their coordination facilities. Nomad servers are infrastructure used by a VMM provider, not Aseman's application authority. |
| Worker | A host running a scheduler client and, where required, the restricted `aseman-vmm-agent`. |
| Workload | A managed executable instance: container, microVM, WASM instance, JavaScript runtime, or another declared runtime. “VM” is a synonym in operation names and wire fields that apply to every workload. |
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
| Store | A shared message space: its members, their permissions, and its signal history. |
| Operation | One entry of the node's router: a path such as `/creatures/create`, the A402 action it is authorized as, its packet guard, and where a signed packet runs it. |
| Signed-packet transports | The TCP, WebSocket, federation, and chain transports: length-prefixed frames carrying a creature's id, its signature over the payload, and an operation path. Framing only; they call the router. |
| Packet guard | How a signed packet authenticates for an operation: `public`, `user`, `store` (a member of the addressed store), or `finance` (a real signature, never the applet marker). |
| Applet marker | `#appletsign`: the signature a machine creature presents for itself from inside the node (a guest call, or a packet the chain ordered). |
| Legacy id | A creature's, store's, or program's account-style string id, `<n>@<origin>`: the identity the signed-packet protocol and the wire views use. Its capsule id is derived from it, and the persisted `core.legacy_identity` index maps one to the other. |
| Artifact ID | `A###`: a specification's identifier, cataloged in `docs/reference/artifacts.md`. |
| Defect ID | `LD-##`: a Caspar-era defect and its resolution, in `docs/reference/defects.md`. |

## Caspar names

| Caspar name | Aseman name |
|---|---|
| Caspar / `caspar-node` | Aseman / `aseman-node` |
| `casparctl` | `asemanctl` |
| `caspar-client` | `aseman-client` |
| `CASPAR_*` and bare Caspar environment variables | `ASEMAN_*` typed configuration; `aseman-config` refuses a retired name with the canonical key to set (`contracts/config/retired-names.json`) |
| `caspar-vm-*` runtime plugins | `aseman-vm-*` runtime plugins of the native VMM backend |
| VM manager / embedded VMM | The Aseman VMM service and its backend |
| shell action | Operation |
| database host call | Signed guest-data proxy operation |
| RocksDB/QuestDB records | Imported by `asemanctl storage migrate` into the storage module's models |
| signaler | The node's signal hub (`live/`) over durable topics |
| hashgraph core | Hashgraph consensus provider |

## Naming rules

1. New package, binary, environment, API, schema, and documentation names use
   `Aseman`/`aseman`/`ASEMAN`.
2. *Legacy* names only data or protocol that exists: legacy ids and their index,
   the Caspar layouts the import reads, and persisted format tags (ADR 0039).
3. A type name states its authority: for example `DesiredWorkload`,
   `ObservedAllocation`, and `CreatureDatabaseBinding` are not interchangeable IDs.
4. “Database” and “namespace” refer to provider-native isolation. “Capsule” refers to
   portable logical data, not a required shared physical table.
5. “Supported” means an operation has executable tests; code presence alone is
   insufficient.
