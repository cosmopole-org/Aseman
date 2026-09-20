---
status: ACCEPTED
owner: security
source_of_truth: plan/migration/05-security-and-authority.md and ADR 0001
last_verified_commit: 800df24076c7
verification: property, adversarial, fuzz, and authorization suites in owning phases
---

# Threat model

## Protected assets

Identity keys, policy/grant state, creature data and schemas, provider credentials,
desired workload state, secrets, module artifacts, node identity, audit history,
usage samples, wallet journals, and consensus finality are protected assets.

## Adversaries

- A malicious or compromised workload, including one knowing another workload or
  creature identifier.
- An authenticated user, operator, node, or module exceeding granted authority.
- A remote node replaying, redirecting, or altering federation messages.
- A malicious/compromised provider returning stale, malformed, or cross-tenant data.
- An attacker controlling network paths but not current private keys.
- Accidental cross-tenant leakage through pools, caches, cursors, logs, backups, or
  migration tooling.
- A stale former leader or replica acting after failover.

## Required controls

1. Canonical audience-bound signatures, short freshness windows, nonces, replay
   state, explicit key epochs, rotation, and revocation.
2. One policy decision for every sensitive action, with default deny and complete
   actor/action/resource/condition audit context.
3. Attenuating delegation: child rights are the intersection of requested,
   currently delegable parent, and administrator policy rights.
4. Signed modules, least-privilege manifests, isolated processes/containers, scoped
   secret delivery, and negotiated contracts.
5. Provider-native creature databases/namespaces and dedicated roles. Server-side
   trusted bindings, bounded database-partitioned pools, transaction-scoped role
   assumption, mandatory reset/verification, and discard-on-ambiguity.
6. Deny-by-default workload ingress/egress and a restricted worker agent; production
   does not expose unrestricted host execution.
7. Signed federation envelopes and responses with destination authorization, expiry,
   hop limits, and deduplication.
8. Fenced coordination for singleton effects and idempotency for retried effects.
9. Integrity-protected append-only audit and finance records, with redacted logs and
   support bundles.

## Explicit non-goals and residual risks

- The system cannot protect plaintext processed by a fully compromised authorized
  provider; providers are isolated, permissioned, observed, and replaceable.
- Side-channel resistance and confidential-computing guarantees are not claimed.
- Availability during a quorum/storage partition is subordinate to preventing split
  authority and unauthorized writes.
- Provider-specific guest schemas may be intentionally non-portable, but activation
  must disclose that fact and migration cannot silently discard them.

## Mandatory abuse cases

Tests must cover identity/database/role spoofing; cross-creature query, catalog,
cursor, cache, pool, log, backup and error leakage; replay and stale keys; confused
deputy delegation; module privilege escalation; malicious descriptors; duplicate
settlement; stale fencing tokens; and migration rollback after partial completion.
