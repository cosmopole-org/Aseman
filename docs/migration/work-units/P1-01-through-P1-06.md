---
status: ACCEPTED
owner: migration/phase-1
source_of_truth: plan/migration/15-agent-execution-guide.md
last_verified_commit: 800df24076c7
verification: cargo xtask fast plus cargo test -p caspar-node --lib
---

# Phase 1 boundary work record

## Completed slices

- P1-01: one root workspace, pinned toolchain, root lockfile, workspace policy, and
  canonical `aseman-node` binary; `caspar-node` is a warning compatibility shim.
- P1-02: `cargo xtask arch|fast|full`, protected-layer dependency rules, scoped
  formatting/Clippy, repository instructions, and common-change playbooks.
- P1-03: generated A103 JSON Schema and a complete 115-key legacy alias/expiry map;
  node composition, telemetry, cluster, CLI, and runtime settings now enter through
  typed `aseman-config` snapshots. Direct process-environment reads outside that crate
  are rejected by characterization, and canonical/legacy conflicts fail closed.
- P1-04: typed IDs, workload states/generations, money, creature database binding,
  store permissions, and signal-tag/query semantics in the I/O-free domain crate.
- P1-05: workload repository/policy/VMM/clock ports, authorized desired-state use case,
  and transport-neutral diagnostic use cases. Legacy route/storage compatibility
  helpers have one owner in contracts; duplicate node implementations were deleted.
- P1-06: TCP and WebSocket now delegate authentication shortcuts, verified-identity
  rate limiting, action dispatch, response mapping, listener attachment, and gateway
  subscription effects to one transport-neutral session owner. Their adapters retain
  framing and connection I/O. A source-boundary characterization test prevents either
  transport from reacquiring the orchestration.

## Migration and rollback

The legacy node remains the composition adapter and consumes clean crates. Rollback can
select the preceding commit/binary because physical data and public wire formats have
not changed in this slice. The canonical binary calls the same characterized run path;
the legacy alias only adds a deprecation warning. `.env` is still copied into the
process once for legacy adapters that have not yet gained constructor injection; typed
parsing remains owned by `aseman-config`, and this mutation expires with those adapters.

## Deferred capability migrations

- Remaining action families still migrate one at a time in their capability-owning
  phases; no generic JSON dispatch is accepted as a substitute for typed use cases.
- Finance, Hashgraph, storage, VMM, federation, and CLI bodies retain their later-phase
  removal rows. The creature owner is partitioned now, but those bodies are not claimed
  as replaced or deletable.

Phase 1's workspace-boundary gate is accepted in `docs/migration/phase-1-gate.md`.
