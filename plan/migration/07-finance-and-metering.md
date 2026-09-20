# Finance and Resource Metering

## Separation of concerns

Finance is split into:

- Metering: collect and normalize trusted resource usage.
- Pricing: versioned rate calculation.
- Ledger: reservations, charges, refunds, balances, and journal.
- Consensus: order/finalize financial records.
- Enforcement: grace, suspend, stop, and notification policies.

Hashgraph becomes the default `ConsensusProvider` adapter. Wallet and pricing rules do not depend on Hashgraph types. Provider changes occur at a finalized epoch with checkpoint, reconciliation, and rollback data.

## Ledger

- Append-only double-entry records.
- Integer minor units; no binary floating-point money.
- Idempotency key for every financial mutation.
- Atomic debit/credit or no mutation.
- Traceability from usage sample through price version to ledger entries.
- Capsule persistence with strong transaction requirements.

## Metering loop

Every minute `aseman-meter` asks the normalized VMM usage endpoint for historical/cumulative workload statistics. The Nomad provider reads allocation statistics; other providers supply equivalent normalized samples.

Dimensions include, when supported:

- CPU time.
- Memory byte-seconds.
- Persistent storage byte-seconds.
- Disk read/write bytes or operations.
- Network ingress/egress, with billable egress identified.
- Accelerator/device time.

Raw provider samples are retained with source identity, timestamp, provider cursor, signature/digest, and collection health. Cumulative counters are converted to interval deltas.

The unique settlement identity is:

```text
(workload_id, interval_start, provider_sample_id)
```

Late samples and outages are backfilled. Duplicate collection or message delivery cannot double-charge.

## Settlement flow

1. Collect and validate the usage sample.
2. Persist raw and normalized usage capsules.
3. Resolve the pricing policy/version active for the interval.
4. Calculate a deterministic charge.
5. Atomically settle wallet/hold and journal entries.
6. Publish the settlement event through the outbox.
7. Update the metering cursor.
8. Apply insufficient-funds policy if required.

Insufficient-funds handling is configurable: notify -> grace period -> suspend/pause -> stop. Enforcement calls the VMM through ordinary authorized operations and is fully auditable.

## Reconciliation

The system compares VMM intervals, usage capsules, pricing results, consensus records, and ledger entries. Missing or conflicting intervals are reported without destructive automatic repair. Administrative reconcile commands can generate reviewed corrective entries.

