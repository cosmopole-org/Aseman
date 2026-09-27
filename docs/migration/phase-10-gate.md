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
truth as of this gate: 25 requirements, 0 violations, 16 MET, 9 PARTIAL (their phases
9 or 10 are not accepted), 0 OPEN.

**A completed Phase 3 cutover record.** The development-host operator cutover was
performed and observed after this gate was first written. Phase 3 is accepted and its
requirements (R10–R13) are MET in the generated traceability report. Production rollout
and legacy deletion remain Phase 10 evidence rather than reopening the Phase 3 gate.

## What remains

| Item | Where it is recorded |
|---|---|
| Deployment-scale load, soak, and full chaos execution | A1002 now has a checked nine-scenario manifest, parser property fuzzing, a thresholded HTTP probe, and focused PostgreSQL realtime/metering runs; production reports remain |
| Supply chain, SBOM, signing, provenance gates | A906 now has a checked policy, deterministic SPDX generator, commit-pinned attestation workflow, RustSec audit, and blocking high/critical artifact scan; retained tagged-release and independent verification evidence remain Phase 9 packaging work |
| Execute shadow traffic and canary nodes | A1003's machine-checked decision/abort contract is delivered; execution needs a deployment |
| Execute and score cold-agent comprehension evaluations | The catalog and authority-path drift gate are delivered in P9-05; deployment-independent scored runs remain open |
| Rollback and disaster-recovery drills | Needs a deployment |
| Production observation of the Phase 3 storage cutover and eventual RL-005 deletion | `phase-3-gate.md` and the removal ledger |
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
