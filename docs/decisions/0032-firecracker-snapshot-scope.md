---
status: DECISION
owner: vmm/storage
source_of_truth: this ADR, ADR 0011, A604, A605
last_verified_commit: pending
verification: capability matrix and unsupported-operation conformance
---

# ADR 0032: Firecracker snapshot portability is outside the initial product scope

## Status

Accepted 2026-09-27.

## Decision

The initial Aseman product does not claim Firecracker snapshot portability. Firecracker
pause/resume remains supported where the worker agent and host declare it. Snapshot,
restore, and cross-provider state moves remain explicit unsupported operations unless a
future provider publishes a snapshot format and portability tier satisfying ADR 0011.

This closes the former KVM-host snapshot exercise as **waived/not required**. It is not
recorded as a successful snapshot or portability observation. A future decision that
adds this capability must reopen A604, declare its format and tier, and supply the KVM
and restore evidence before advertising `snapshot=true`.

## Consequences

- Capability negotiation remains truthful: unsupported is an accepted outcome and no
  stop/start or empty-volume recreation may impersonate snapshot/restore.
- P6-06's move planner and refusal behavior remain authoritative.
- No release or cleanup gate depends on access to a KVM-capable test host.

## Rollback

Revert this scope decision and execute the original P6-05/P6-06 KVM portability tier.
