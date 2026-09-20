---
status: ACCEPTED
owner: reliability
source_of_truth: plan/migration/01-target-architecture.md and plan/migration/10-verification-and-acceptance.md
last_verified_commit: 800df24076c7
verification: phase-owned recovery, failover, and chaos suites
---

# Failure model

The design assumes crash-stop processes, delayed/duplicated/reordered messages,
partial network partitions, stale caches, retry after unknown completion, clock skew
within declared bounds, provider unavailability, disk pressure, and operator error.
Byzantine peers are handled only at explicit cryptographic federation and consensus
boundaries; ordinary internal providers are not assumed Byzantine-tolerant.

| Failure | Safe behavior | Recovery evidence |
|---|---|---|
| API/control replica crash | In-flight result may be unknown; durable operation remains reconcilable. | Retry with same idempotency key; another replica resumes with a valid fence. |
| Old leader resumes | Its lease is expired and committed effects reject its fencing token. | Chaos test proves no duplicate singleton effect. |
| VMM/backend timeout | Return pending/unknown, never invent success or failure. | Reconcile by operation ID and desired generation. |
| Worker loss | Mark observations stale/lost; obey workload statefulness policy before reschedule. | Lost-worker and volume-policy scenarios. |
| Storage-provider outage | Bound queues and deadlines; do not acknowledge non-durable writes. | Restart/replay/checkpoint tests. |
| Storage cutover interruption | Exactly one recorded authority generation; ambiguous state fails closed. | Resume/rollback from migration journal and semantic comparison. |
| Guest pool cancellation/error | Role/session is reset and verified or connection is discarded. | Cross-creature pool-contamination adversarial test. |
| Module crash or bad activation | Health fails; supervisor drains/rolls back routing generation. | Stage/activate/drain/rollback conformance test. |
| Federation partition | No local impersonation of remote authority; bounded retry/circuit break. | Expiry/replay/dedupe and partition scenarios. |
| Realtime duplicate/gap | Consumers deduplicate; durable offsets expose and resume gaps. | Replay/restart/retention tests. |
| Meter duplicate/late sample | Stable interval identity deduplicates; correction is explicit. | Golden settlement and late-sample fixtures. |
| Key revocation during flight | Epoch and decision-time rules produce deterministic deny/accept; audit records rule. | Rotation/revocation boundary tests. |
| Disk/resource exhaustion | Backpressure and readiness degradation precede unsafe admission. | Capacity thresholds and recovery runbook. |

No correctness claim depends on process-local memory surviving restart. Every queue,
cache, retry, pool, and compatibility path must state bounds and overload behavior.
