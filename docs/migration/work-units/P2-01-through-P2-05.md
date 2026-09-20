---
status: ACCEPTED
owner: migration/phase-2
source_of_truth: plan/migration/15-agent-execution-guide.md
last_verified_commit: 800df24076c7
verification: cargo xtask full plus sample-provider supervisor_lifecycle
---

# Phase 2 module-platform work record

## Completed slices

- P2-01: closed manifest, package, permission, trust, lifecycle, placement, and bootstrap
  schemas; protobuf control/provider contracts; generated Tonic bindings; frozen field
  numbers and RPC names; and an authenticated administration OpenAPI edge.
- P2-02: explicit Ed25519 publisher enrollment/revocation, manifest-and-payload-bound
  signatures, platform/path/size checks, collision-detecting atomic artifact cache, and
  signed expiring atomic bootstrap snapshots.
- P2-03: configuration ownership, conformance-before-stage, exact permission/secret
  launcher receipts, readiness cleanup, monotonic routing generations, drain, restore,
  stop, and rollback. The initial node launcher supports zero-host-permission providers
  and rejects every broader declaration until a platform sandbox implements it.
- P2-04: node and standalone authenticated administration adapters, byte-upload install,
  all lifecycle CLI commands, deterministic status output, and a placement/quorum
  reconciliation engine. Cluster-scoped mutation fails explicitly until the Phase 3
  authoritative capsule registry can persist desired/observed placement; it never
  degrades to an unsafe one-node action.
- P2-05: reusable conformance crate and a real Tonic sample provider exercising
  negotiation, health, bounded requests, deadlines, cancellation, lifecycle RPCs, and
  two-version process switch/rollback from signed `.amod` bundles.

## Migration and rollback

This phase adds a parallel provider path and does not replace a production capability.
Operators enroll a publisher, upload a signed bundle, validate and stage it, then
activate a routing generation. The prior ready process remains alive; rollback requires
its restore acknowledgement before routing changes. A preceding node binary ignores the
new cache/contract paths and continues using embedded providers.

Standalone administration starts only when an explicit auth token exists. Clustered
nodes reuse the authenticated cluster listener. Cluster-scope requests return `501`
until the capsule-backed desired-state owner is present; callers cannot mistake local
success for quorum placement.

## Removal status

RL-014 remains open. The sample proves the replacement mechanism, not parity for any
embedded VM runtime. Phase 5 must migrate runtime providers and Phase 10 must satisfy the
compatibility deletion window before compile-time aggregation can be removed.
