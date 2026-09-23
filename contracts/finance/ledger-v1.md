---
status: ACCEPTED
owner: finance/ledger
source_of_truth: this contract, aseman-domain::finance
last_verified_commit: eebb9c5
verification: cargo test -p aseman-domain finance; live_finance on PostgreSQL 16
---

# A803: the ledger (v1)

## Double entry, append-only

A journal record is a set of entries whose amounts sum to **zero**. A penny short is not
a record and is refused before anything is written — the live test asserts a refused
record leaves no half-entry behind.

Records are never edited. A charge that was made and then returned is two facts, not
none.

## Idempotency

Every financial mutation has an idempotency key. For a settlement it is the settlement
identity itself.

**Committing the same key twice is success, not a second record.** That is the whole
mechanism: a retry after a crash, an outage, or a duplicated message lands on the same
key and changes nothing. The live test commits every settlement three times and asserts
the balance is exactly one charge per interval.

## Accounts

An account is a name. A balance is the sum of its entries — a query, not a stored
number that could drift from the journal behind it.

By convention: `wallet:{creature}` for what a creature holds, `revenue:{service}` for
what the node has earned, `hold:{key}` for money set aside.

## Holds

A hold sets money aside before work is done, so several concurrent workloads cannot
spend the same balance twice over.

- A hold is **captured** for what was actually used, or **released**.
- A reservation is a **ceiling**: capturing more than was held is refused.
- A hold that has been captured or released is finished. Capturing it again is refused —
  that double charge is the mistake holds exist to prevent.

## Refunds

A refund is a new balanced record that reverses the original's entries, carrying the
same price version so it traces to the same price as the charge it reverses. It never
edits or deletes the original.

## Settlement states

```text
interval metered -> priced -> settled (journal record committed)
                          \-> refunded (a second record)
hold placed -> captured (a journal record) | released
```

Every state transition is a record, and every record is idempotent.
