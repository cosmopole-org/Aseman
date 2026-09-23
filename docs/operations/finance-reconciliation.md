---
status: CURRENT
owner: operations/finance
source_of_truth: A806, aseman-domain::finance
last_verified_commit: eebb9c5
verification: cargo test -p aseman-domain finance
---

# Reconciling metering against the ledger

Written for: the operator investigating a billing discrepancy.

## The principle

**Nothing repairs automatically.** A reconciler that silently moves money turns one bad
charge into an unauditable series of them. Every corrective entry is proposed for a
person to review, and applied as a new balanced journal record — never as an edit.

## What is compared

Intervals, price lists, journal records, and consensus finalizations. Four kinds of
discrepancy come out:

| Discrepancy | Meaning | Proposal |
|---|---|---|
| `unsettled` | An interval has no journal record | **None.** Settling it is the ordinary path, not a correction |
| `unexplained` | A journal record matches no interval | **None.** Money that arrived from nowhere is a question, not an arithmetic problem |
| `unknown_price` | A charge names a price version no longer published | **None.** There is nothing to recompute against |
| `mispriced` | A settled amount differs from what its interval prices to now | The difference, as a balanced correction |

Only the last has a defensible arithmetic answer. The other three are escalated.

## Running it

1. `unsettled()` lists intervals with no record. If the metering loop is healthy these
   settle on the next pass; if they persist, the loop is stuck and that is the problem
   to fix.
2. Compare each settled record against its interval repriced at the version the record
   names. A difference is `mispriced`.
3. Records whose settlement key matches no interval are `unexplained`. Do not adjust
   them. Find out where they came from.
4. For each `mispriced`, review the proposed correction and apply it as a normal
   idempotent journal commit. Its key is `correction:{settlement key}`, so applying it
   twice is a no-op.

## What a clean run means

`Reconciliation::clean()` is true when nothing is unsettled, unexplained, or unpriced.
That is the state to return to, and the state to alert on leaving.

## What it does not do

It does not delete, it does not edit, and it does not reprice history. A price list is
never edited after publication precisely so that repricing an old interval is a
detectable disagreement rather than a silent rewrite.
