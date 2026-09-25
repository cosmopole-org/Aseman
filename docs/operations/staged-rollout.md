---
status: CURRENT
owner: release/operations
source_of_truth: contracts/deploy/rollout-policy.json
verification: python3 scripts/check_rollout_policy.py --check
---

# Shadow and canary rollout

This is A1003's decision procedure. It defines whether a rollout may advance; it is not
evidence that a rollout occurred. Each execution records the release digest, old and new
provider generations, host/profile identity, start and end times, request counts,
metric exports, operator, decision, and rollback result outside the source tree.

## Shadow

Send the same admitted request to the candidate in effect-suppressed comparison mode.
Reads compare canonical response semantics. Mutations compare resolved resource,
authorization decision, intended capsule/journal delta, and audit record without
committing the candidate effect. Credentials, secrets, bodies, and tenant identifiers
must not enter comparison labels or logs.

Observe for at least the duration and counts in `rollout-policy.json`. Any unauthorized
acceptance, duplicate effect, missing audit record, or semantic mismatch aborts. A p95
regression above the declared threshold also aborts unless a reviewed release exception
records the security/correctness tradeoff and an expiry.

## Canary

Advance only through the declared traffic stages. A stage starts after the candidate is
ready and lasts for both the minimum duration and request count. Compare candidate and
stable cohorts on the same host class and workload mix. Zero-tolerance correctness and
security signals abort immediately. Error-rate, readiness, and latency thresholds use
the checked contract values.

## Abort and rollback

On abort, stop assigning new work to the candidate, preserve evidence, activate the
previous listener/provider generation, drain candidate connections, and verify health,
audit continuity, idempotency outcomes, storage binding generation, realtime offsets,
and finance balance. Never truncate the target or erase an incomplete migration as
rollback. Run every verification command in the contract and attach its output to the
rollout record.

Advancing to 100 percent is not deletion approval. ADR 0004 observation and every
removal-ledger deletion gate remain independent requirements.
