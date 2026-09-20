---
status: ACCEPTED
owner: migration/phase-1
source_of_truth: plan/migration/09-migration-phases.md
last_verified_commit: 800df24076c7
verification: cargo xtask full
---

# Phase 1 exit gate

## Decision

Accepted 2026-09-20. Phase 2 module-runtime work may begin. This gate accepts the
workspace and dependency boundaries; it does not declare legacy capabilities migrated
or authorize deleting adapters whose later replacement and deletion gates remain open.

## Evidence

- A101 establishes the root workspace, pinned toolchain, root lockfile, workspace lint
  policy, canonical `aseman-node` binary, and expiring `caspar-node` alias.
- A102 is enforced by `cargo xtask arch|fast|full`; domain, ports, and application have
  no driver/framework dependencies, and clean crates pass scoped formatting and Clippy
  with warnings denied.
- A103 defines the typed configuration schema and all 115 observed legacy aliases.
  Canonical/legacy conflicts fail closed and direct process-environment reads outside
  `aseman-config` are rejected by characterization.
- A104 and A105 are generated from the public domain values and behavioral ports.
- A106 documents adding configuration, ports/adapters, and application use cases.
- Typed workload state, authorization, VMM, time, server-identity, and peer-directory
  seams have transport-neutral application tests and one owner in the clean crates.
- TCP and WebSocket framing adapters share one session orchestrator; route decoding,
  authentication shortcuts, rate limiting, dispatch, response mapping, subscription,
  and listener attachment are no longer duplicated.
- The creature action owner is partitioned by responsibility. Public route and call-path
  generators recurse through action-family submodules and prove all 77 actions remain
  represented one-to-one.

## Verification

```bash
cargo xtask full
```

The accepted run passed 10 characterization tests, 23 clean-crate unit tests, 379 node
library tests, and 33 CLI tests. The node and CLI retain one pre-existing non-fatal
compiler warning each; warning denial is enforced for new clean crates.

## Deferred replacement and deletion gates

- RL-002 through RL-004 remain open for type/use-case families that still live in the
  legacy node. Their owning phases must migrate each family before deleting its body.
- Finance action internals and Hashgraph remain Phase 8 work; the Phase 1 split changes
  ownership layout only.
- Storage, guest data, policy, VMM, federation/realtime, and CLI compatibility removals
  remain governed by Phases 3 through 10 and ADR 0004.
- The temporary typed-config-to-process compatibility snapshot expires only when its
  remaining legacy adapters accept constructor-injected configuration.

Rollback remains the preceding node binary/commit because this phase changes no public
wire format or physical persisted-data layout.
