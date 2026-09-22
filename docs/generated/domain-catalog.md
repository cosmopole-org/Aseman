---
status: GENERATED
owner: architecture/phase-1
source_of_truth: crates/aseman-domain/src/lib.rs
last_verified_commit: 800df24076c7
verification: python3 scripts/generate_phase1_contracts.py --check
---

# Domain catalog

| Domain type/state machine | Owning crate |
|---|---|
| `BindingStatus` | `aseman-domain` |
| `CreatureDatabaseBinding` | `aseman-domain` |
| `DesiredWorkload` | `aseman-domain` |
| `DesiredWorkloadState` | `aseman-domain` |
| `DomainError` | `aseman-domain` |
| `Generation` | `aseman-domain` |
| `Money` | `aseman-domain` |
| `ObservedWorkloadState` | `aseman-domain` |
| `OperationState` | `aseman-domain` |

This catalog is generated from public declarations. Semantic guarantees remain
in the source documentation, accepted ADRs, and conformance tests.
