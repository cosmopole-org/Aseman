---
status: ACCEPTED
owner: finance/consensus
source_of_truth: this contract, aseman-domain::consensus, aseman-ports::consensus
last_verified_commit: eebb9c5
verification: cargo test -p aseman-domain consensus
---

# A804: consensus epochs and provider switching (v1)

## What consensus is for here

Finance does not depend on a consensus implementation. Wallets, pricing, and the ledger
are decided without one. What consensus adds is an **order several nodes agree on**, and
a point past which that order will not change.

That is why `ConsensusProvider` is a port and Hashgraph is an adapter behind it.

## Epochs

An epoch is finalized and monotonic. **A finalized epoch never reopens**, and a
finalized order **only ever extends**: a finalization that would reorder or replace
something a node has already acted on is refused, because the ledger entries behind it
are already committed.

A record's place in the total order is `(epoch, position)`.

## Disagreement

A finalized record carries a digest of what consensus agreed on. A node whose own record
disagrees **reports it**. It never repairs automatically: two nodes disagreeing about
money is a thing a person must look at.

## Switching provider

A provider changes **only at a finalized epoch**, and only when:

1. The checkpoint describes **that** epoch. A checkpoint of some other moment does not
   describe what the incoming provider would inherit.
2. **Nothing is in flight.** Records submitted but not finalized would be ordered by
   neither provider — the outgoing one has stopped, the incoming one never saw them.
3. The checkpoint's digest is a real SHA-256 digest over the finalized order, so the
   incoming provider can be shown to have inherited the same history.

An incoming provider that has already finalized something of its own refuses to adopt a
checkpoint: adopting then would fork the order.

Swapping mid-epoch would leave records ordered by one provider and records ordered by
another with nothing relating them, and money that cannot be put in an order cannot be
reconciled.
