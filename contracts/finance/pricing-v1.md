---
status: ACCEPTED
owner: finance/pricing
source_of_truth: this contract, aseman-domain::finance
last_verified_commit: eebb9c5
verification: cargo test -p aseman-domain finance
---

# A802: pricing (v1)

## Money

Integer **minor units**, signed. Never a binary float: a float cannot represent a tenth,
and a charge off by a ten-thousandth of a unit every minute is a charge nobody can
reconcile. Addition and subtraction are checked; money never wraps.

## Rates

A rate is minor units per unit of a dimension, scaled by `PRICE_SCALE` = **1,000,000,000**.

A billion, not a million, because per-byte prices are small: at a million, "one minor
unit per mebibyte" truncates to a rate of zero and the dimension silently becomes free.
That mistake was made while building this, which is why a price list whose rate rounds
to zero is **refused at publication** rather than discovered at reconciliation.

## Rounding

Up. A fraction of a minor unit consumed is a minor unit owed; rounding down would make a
busy workload free in the small.

## Determinism

The same interval and the same price list always produce the same charge, on any node,
at any time. Integer arithmetic throughout, no clock, no locale.

A charge carries its **lines**: what each dimension contributed, so it can be explained
one dimension at a time.

## Versions

A price list has a version and an instant it takes effect from. The list in force for an
interval is the newest one that had taken effect at the interval's start.

A published price list is **never edited** — charges refer to it by version — and
publishing the same version twice is refused.

## Unpriced dimensions

A billable dimension the price list does not name is an **error**, not a free ride.
Silently charging zero would be a revenue hole nobody notices.
