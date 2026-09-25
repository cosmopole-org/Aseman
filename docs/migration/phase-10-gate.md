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

**A mechanically checked requirements traceability report (A1005).**
`scripts/generate_requirements_traceability.py` verifies every requirement in
`plan/migration/14-plan-integrity-and-traceability.md` names a design authority, a
delivery phase, and an acceptance authority, maps each requirement's phases to their
phase-gate status, and runs `--check` in `cargo xtask fast`. The report records the
truth as of this gate: 25 requirements, 0 violations, 12 MET, 13 PARTIAL (their phases
3, 9, or 10 are not accepted), 0 OPEN.

**A corrected Phase 3 record.** `status.md` previously listed Phase 3 as accepted while
`phase-3-gate.md` correctly recorded it as in progress (the switch is an operator
action). The status now agrees with the gate: Phase 3 is **ready, not switched**, and
its requirements (R10–R13) are PARTIAL in the traceability report until the cutover.

## What remains

| Item | Where it is recorded |
|---|---|
| Fuzz, load, and full chaos suites | Acceptance: "Required suites" |
| Supply chain, SBOM, signing, provenance gates | Phase 9 packaging |
| Execute shadow traffic and canary nodes | A1003's machine-checked decision/abort contract is delivered; execution needs a deployment |
| Execute and score cold-agent comprehension evaluations | The catalog and authority-path drift gate are delivered in P9-05; deployment-independent scored runs remain open |
| Rollback and disaster-recovery drills | Needs a deployment |
| The Phase 3 storage cutover | `phase-3-gate.md`: an operator action (runbook step 7) |
| Deleting the legacy paths | The removal ledger's outstanding rows, each with its blocker |

Most need either infrastructure outside this repository or a running deployment. The
remaining hierarchy and legacy cleanup additionally need their named replacement and
deletion gates; none is blocked on undocumented design.

## The rule this phase exists to protect

A replacement gate proves the new path; a deletion gate removes the old one. A
capability with two authoritative implementations is worse than one with an old
implementation, because nobody can tell which is true.

This migration has **not** finished deleting. What it has done is make the remaining
deletions visible, attributed, and enforced — so the next person inherits a list rather
than an archaeology problem.
