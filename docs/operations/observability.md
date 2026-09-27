---
status: OPERATIONAL
owner: operations
source_of_truth: contracts/observability/policy-v1.json, contracts/realtime/capacity-v1.json
verification: python3 scripts/check_observability_policy.py --check
---

# Observability, SLOs, and capacity

The A905 contract defines the stable Prometheus metric names, bounded labels, four
30-day SLOs, and error-budget policy. The checked Grafana dashboard is
`deploy/observability/grafana/aseman-overview.json`; checked Prometheus rules are in
`deploy/observability/prometheus/aseman-alerts.yml`. Request, trace, user, creature,
workload, and event IDs belong in structured logs and traces, never metric labels.

Planning defaults are not measurements. Before production approval, run the A1002 load
scenarios against the intended topology, fill the capacity worksheet below from the
result, and retain the report with the release evidence. A missing value fails
admission; it is not replaced by a guessed zero or by the planning default.

## Release freeze

Freeze promotion when a page-level error-budget alert is active, the 30-day budget is
exhausted, realtime dead letters are nonzero, a durability/authorization invariant is
violated, or the A708 capacity rules do not pass. Roll back a canary on any zero-
tolerance signal in `contracts/deploy/rollout-policy.json`.

## Capacity worksheet

Record the release, topology, test duration, dataset, and these measured values:

| Input | Measured value |
|---|---|
| Peak events/second | |
| p95 event bytes | |
| Sustained publish events/second | |
| Sustained replay events/second | |
| PostgreSQL available bytes | |
| PostgreSQL max connections | |
| API replicas / publishers / migration workers / operator reserve | |

Evaluate every admission rule and formula in `contracts/realtime/capacity-v1.json`.
Size for the larger of retained-data demand and replay demand, with the specified 2x
headroom. Re-run after an event schema, retention, topology, PostgreSQL major version,
or workload-shape change.

<a id="api-error-budget-burn"></a>
## API error-budget burn

Confirm the failure is not a monitoring gap by comparing request logs and readiness.
Segment the dashboard by route and dependency, stop promotion, and roll back the canary
if the burn began with it. If not, shed optional work, preserve mutation idempotency,
and page the owning service. Close only after both alert windows recover.

<a id="service-not-ready"></a>
## Service not ready

Inspect the health report's required dependency checks. Do not restart a healthy
dependency blindly. Keep liveness separate from readiness; remove the replica from
traffic, repair the named dependency, then require readiness to remain healthy for ten
minutes before restoring traffic.

<a id="realtime-backlog"></a>
## Realtime backlog

Check PostgreSQL latency, connection saturation, outbox claim age, active publisher
lease, and publish failure rate. A growing backlog with healthy PostgreSQL calls for
publisher capacity; an unhealthy database calls for database recovery. Preserve the
outbox and checkpoints during rollback. Never skip or renumber events.

<a id="realtime-dead-letters"></a>
## Realtime dead letters

Freeze promotion immediately. Identify the failing destination and payload version,
retain the event IDs in restricted incident evidence, repair the consumer, and replay
from the durable outbox. Do not mark rows published by hand. Any loss, duplicate
sequence, or checkpoint regression is a release abort.

<a id="finance-settlement-lag"></a>
## Finance settlement lag

Follow `docs/operations/finance-reconciliation.md`. Preserve raw samples and pricing
versions, verify the fenced settlement worker, and backfill idempotently. Never bypass
the consensus or balanced-journal requirements to clear the alert.

<a id="vmm-reconciliation-drift"></a>
## VMM reconciliation drift

Compare desired generation, observed generation, and the current operation. Check the
selected backend and worker health. Reconcile the same workload identity and generation;
do not create a replacement identity to make the count disappear.
