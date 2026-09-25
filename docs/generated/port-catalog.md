---
status: GENERATED
owner: architecture/phase-1
source_of_truth: crates/aseman-ports/src/lib.rs
last_verified_commit: 800df24076c7
verification: python3 scripts/generate_phase1_contracts.py --check
---

# Port catalog

| Behavioral port | Owning crate |
|---|---|
| `ActionExecutor` | `aseman-ports` |
| `BlobStore` | `aseman-ports` |
| `CanonicalRecordWriter` | `aseman-ports` |
| `ChallengeStore` | `aseman-ports` |
| `ClockPort` | `aseman-ports` |
| `CreatureBalances` | `aseman-ports` |
| `CreatureDatabaseBindings` | `aseman-ports` |
| `CreatureDirectory` | `aseman-ports` |
| `CreatureMetadata` | `aseman-ports` |
| `CreatureTypes` | `aseman-ports` |
| `DecisionAudit` | `aseman-ports` |
| `EntityDirectory` | `aseman-ports` |
| `GatewayRoutes` | `aseman-ports` |
| `GrantStore` | `aseman-ports` |
| `GuestKv` | `aseman-ports` |
| `IdentityVerifier` | `aseman-ports` |
| `KeyDirectory` | `aseman-ports` |
| `MigrationRecordSource` | `aseman-ports` |
| `MigrationStateStore` | `aseman-ports` |
| `PeerDirectoryPort` | `aseman-ports` |
| `PolicyDecisionPort` | `aseman-ports` |
| `ProgramAlarms` | `aseman-ports` |
| `ProgramDirectory` | `aseman-ports` |
| `ProgramMetadata` | `aseman-ports` |
| `PublicActionIdempotency` | `aseman-ports` |
| `ReplayGuard` | `aseman-ports` |
| `ServerIdentityPort` | `aseman-ports` |
| `SessionDirectory` | `aseman-ports` |
| `SignalLog` | `aseman-ports` |
| `StoreAccess` | `aseman-ports` |
| `StoreDirectory` | `aseman-ports` |
| `StoreMetadata` | `aseman-ports` |
| `VmResourceEntities` | `aseman-ports` |
| `VmResourceStores` | `aseman-ports` |
| `WorkloadRepository` | `aseman-ports` |

This catalog is generated from public declarations. Semantic guarantees remain
in the source documentation, accepted ADRs, and conformance tests.
