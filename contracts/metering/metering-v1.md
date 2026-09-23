---
status: ACCEPTED
owner: finance/metering
source_of_truth: this contract, aseman-domain::finance
last_verified_commit: eebb9c5
verification: cargo test -p aseman-domain finance; live_finance on PostgreSQL 16
---

# A801: normalized metering (v1)

## The unit of settlement

```text
(workload_id, interval_start, provider_sample_id)
```

A settlement is that triple, and it is the journal's idempotency key. Collecting the
same provider reading twice, or delivering the same message twice, therefore cannot
charge twice — the second attempt is the same identity, not a duplicate to be detected.

## Dimensions

`cpu_millis`, `memory_mib_seconds`, `storage_mib_seconds`, `disk_read_bytes`,
`disk_write_bytes`, `network_ingress_bytes`, `network_egress_bytes`,
`accelerator_millis`.

**Ingress is metered and never billable.** A workload cannot refuse what is sent to it,
so charging for it would let anyone on the internet spend a creature's balance. A price
list that names it is refused.

## Cumulative to delta

Providers report cumulative counters. The meter turns two consecutive readings of one
workload into the interval between them.

- Two readings of **different workloads** are not an interval.
- A reading at or before the one it follows is not an interval: time must move.
- A counter that went **backwards** means the workload restarted and its counters reset.
  The delta is read as the new counter itself. Treating the underflow as unsigned would
  produce an astronomical charge.

## Clock, skew, and late samples

Interval bounds come from the provider's collection timestamps, not the node's clock: a
charge must be explainable from the reading that produced it.

A sample collected late is settled when it arrives. Ordering does not matter, because
the settlement identity does not depend on arrival order — an outage backfills by
replaying the samples, and each settles exactly once.

## Retention

Raw samples are retained with their provider, sample identity, and timestamp. A charge
must be explainable years later from the reading behind it, not merely from the
derived interval.
