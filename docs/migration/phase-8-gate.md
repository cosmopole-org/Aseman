---
status: ACCEPTED
owner: migration/phase-8
source_of_truth: plan/migration/09-migration-phases.md (Phase 8), plan/migration/07-finance-and-metering.md
last_verified_commit: eebb9c5
verification: cargo test -p aseman-domain finance; live_finance on PostgreSQL 16
---

# Phase 8 exit gate

## Decision

**Accepted, with consensus ordering owned onward.** Metering, pricing, the ledger, and
enforcement are separate ports. Settlement is idempotent by construction, and every
charge traces to a raw sample and a price version.

## Gate clauses

| Clause | Evidence |
|---|---|
| Crash and retry produce no duplicate settled intervals | `live_finance`: each of five settlements is committed **three times**; the balance is exactly five intervals. Committing a key twice is success and changes nothing |
| Crash and retry produce no missing settled intervals | The same test: `unsettled()` is empty after every pass, including after a late out-of-order interval |
| Every charge traces to a raw usage sample and a price version | The idempotency key *is* the settlement identity `(workload, interval start, provider sample)`, and every record carries its `price_version`; both are asserted |

## The mechanism, in one line

The settlement identity is a primary key. A duplicate collection, a duplicated message,
or a retry after a crash is not *detected* as a duplicate — it **is** the same row, so
there is nothing to detect.

## Decisions worth stating

**Integer minor units, never a float.** A binary float cannot represent a tenth. A
charge off by a ten-thousandth of a unit every minute is a charge nobody can reconcile.

**Ingress is never billable.** A workload cannot refuse what is sent to it, so charging
for it would let anyone on the internet spend a creature's balance. The dimension exists
and is metered; it simply cannot carry a price, and a price list that tries is refused.

**A billable dimension the price list forgot is an error.** Charging zero silently would
be a revenue hole nobody notices. Pricing refuses and names the dimension.

**A rate that rounds to zero is refused at publication.** The scale is a billion, not a
million, because per-byte prices are small: at a million, "one minor unit per mebibyte"
truncates to a rate of zero and the dimension silently becomes free. That mistake was
made and caught while building this — hence the check.

**A counter that went backwards means the workload restarted.** The delta is read as the
new counter, not as an unsigned underflow, which would be an enormous charge.

**A fraction of a minor unit consumed is a minor unit owed.** Rounding down would make a
busy workload free in the small.

**A dip below zero does not stop anyone's workloads.** A balance that goes negative
between a charge and a top-up is normal. Enforcement is notify, then grace, then
suspend — and suspension goes through ordinary authorized VMM operations, so it is
auditable.

## Required artifacts

A801 through A807 are all accepted: metering (`contracts/metering/metering-v1.md`),
pricing, the ledger, consensus, and enforcement (`contracts/finance/`), reconciliation
(`docs/operations/finance-reconciliation.md`), and the six golden usage-to-journal cases
that run in the gate and fail when a fixture value changes.

Holds, refunds, the full notify/pause/stop escalation, and corrective-entry rules were
added after an audit found them missing behind a single lumped status row — which is
why `scripts/check_artifact_register.py` now refuses a combined or non-standard state.

## Owned by later phases

- **Hashgraph composition and a live switch.** The engine and its
  `ConsensusProvider` adapter are delivered at `modules/consensus/hashgraph`; the
  adapter uses the real Babble proxy and committed blocks, and its 213 engine/adapter
  tests pass. Wiring the provider into settlement and observing a checkpointed switch
  on the live peer mesh remain (RL-011).
- **Broader meter dimensions and enforcement composition.** `apps/aseman-meter` now
  pages A501 workloads, collects the endpoint's cumulative CPU/network counters,
  persists samples and intervals, backfills unsettled intervals, resolves the wallet
  from the server-returned creature label, and commits idempotent settlements. A501's
  memory/storage values are gauges, so the meter deliberately does not pretend they are
  cumulative byte-seconds; those dimensions remain pending a richer provider sample.
- **Reconciliation reports** as an operator command: `Reconciliation` is defined and
  deliberately reports rather than repairing, since destructive automatic repair of
  money is never right.
