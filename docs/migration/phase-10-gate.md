---
status: PARTIAL
owner: migration/phase-10
source_of_truth: plan/migration/09-migration-phases.md (Phase 10), plan/migration/10-verification-and-acceptance.md
last_verified_commit: eebb9c5
verification: cargo xtask fast; docs/migration/acceptance.md
---

# Phase 10 exit gate

## Decision

**Not accepted.** Its criterion is "every criterion in the acceptance document passes
and rollback drills are signed off". `docs/migration/acceptance.md` records which do and
which do not, and several do not. Marking this gate accepted would make the acceptance
document decorative.

## What this phase did deliver

**The enforcement that makes "delete it later" mean something.**
`scripts/check_removal_ledger_due.py` runs in `cargo xtask fast` and fails while a
removal-ledger row whose phase gate has been accepted records no outcome. It found
eleven such rows on its first run — replacements that had been accepted while the
ledger said nothing about the superseded path. Each now records an outcome: deleted,
reduced, closed at the edge, or open with what is in the way.

The check tightens on its own: it reads which phase gates are accepted, so accepting
Phase 9 would immediately make RL-015 and RL-018 overdue. That is deliberate — it is
what stops a gate from being accepted for the sake of a tidy table.

**A truthful acceptance assessment.** Every criterion from the acceptance document is
marked MET with evidence or OPEN with the obstacle.

## What remains

| Item | Where it is recorded |
|---|---|
| Fuzz, load, and full chaos suites | Acceptance: "Required suites" |
| Supply chain, SBOM, signing, provenance gates | Phase 9 packaging |
| Shadow traffic and canary nodes | Needs a deployment |
| Documentation drift and comprehension evaluations as release gates | Acceptance: "Documentation" |
| Rollback and disaster-recovery drills | Needs a deployment |
| Deleting the legacy paths | The removal ledger's outstanding rows, each with its blocker |

Every one needs either infrastructure outside this repository or a running deployment.
None is blocked on design.

## The rule this phase exists to protect

A replacement gate proves the new path; a deletion gate removes the old one. A
capability with two authoritative implementations is worse than one with an old
implementation, because nobody can tell which is true.

This migration has **not** finished deleting. What it has done is make the remaining
deletions visible, attributed, and enforced — so the next person inherits a list rather
than an archaeology problem.
