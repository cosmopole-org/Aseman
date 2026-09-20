---
status: ACCEPTED
owner: migration/phase-2
source_of_truth: plan/migration/09-migration-phases.md
last_verified_commit: 800df24076c7
verification: cargo xtask full plus cargo test -p aseman-sample-provider --test supervisor_lifecycle
---

# Phase 2 exit gate

## Decision

Accepted 2026-09-20. A signed sample provider can be installed, validated, switched,
drained, and rolled back as an independently launched process without rebuilding the
node. Phase 3 capsule/storage work may begin. This decision accepts the module platform;
it does not claim parity for or authorize deletion of any embedded production provider.

## Evidence

- A201–A208 are closed by checked schemas, generated protobuf bindings, ADR 0015,
  lifecycle/placement/bootstrap tests, the conformance kit, and generated catalog.
- Signatures bind the exact manifest and complete package payload. Unknown/revoked keys,
  digest mismatch, traversal, missing SBOM/license/config/executable files, cache
  collisions, and platform mismatch fail before execution.
- The trusted launcher must return an exact permission and secret-reference receipt.
  The composed initial launcher permits only providers declaring no host access.
- The supervisor refuses activation before conformance and readiness, advances routing
  monotonically, retains the previous process, and calls restore before rollback.
- The real-process integration packages the compiled sample binary twice, launches it
  through generated gRPC, switches versions, drains the old generation, rolls back, and
  successfully invokes the restored version.
- `casparctl module` uploads artifact bytes and reaches an authenticated node or
  standalone administration listener. Missing auth disables the surface. Unsafe paths,
  unknown scopes, and cluster scope without a desired-state authority fail closed.
- Cluster placement reconciliation requires independent verification/readiness and
  quorum before a routing-generation advance. Phase 3 composes its desired state into
  the authoritative capsule registry.

## Replacement and deletion gates

The generic module mechanism and sample provider pass their replacement gate. RL-014
does not pass its deletion gate: production runtime/VMM providers remain embedded until
Phase 5 parity and Phase 10 compatibility/removal evidence. Rollback is therefore the
previous routing generation or the preceding node binary; no legacy path was deleted.
