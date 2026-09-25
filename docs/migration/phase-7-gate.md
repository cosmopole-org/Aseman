---
status: ACCEPTED
owner: migration/phase-7
source_of_truth: plan/migration/09-migration-phases.md (Phase 7)
last_verified_commit: eebb9c5
verification: cargo xtask fast; live_two_clusters, live_federation, live_realtime on PostgreSQL 16
---

# Phase 7 exit gate

## Decision

**Accepted, with the transports owned onward.** Two independently administered clusters
federate, the destination decides every operation, replays and retries are told apart,
realtime is durable, and the legacy transports are framing only — checked, not asserted.

## Gate clauses

| Clause | Evidence |
|---|---|
| Two independently administered clusters perform permitted operations across federation | `live_two_clusters`: two clusters with separate databases, directories, and guards; a permitted request crosses and executes exactly once |
| Forbidden operations fail at the destination | The same test: a relational action is denied because **an envelope establishes no relational fact by arriving**; an unknown action is denied rather than guessed; an unknown peer, a misaddressed envelope, and an expired one are each refused — by the destination's own clock and records |
| Legacy transports contain framing only and share one session path | `scripts/check_legacy_transports.py` in `cargo xtask fast`, verified by tampering |
| The default HTTP API exists and is the same application path | `contracts/public/openapi.json`, generated from the action registry: 76 operations, one per registered action |
| Durable realtime with capsule outbox and checkpoints | `live_realtime`: an event and its outbox row are written together; claims, checkpoints, replay bounds, and retention all behave |
| Every federation message type is authenticated and replay-protected | A705: nonce per source node, 60-second maximum lifetime, 4-hop limit; `live_federation` and `live_two_clusters` exercise both the replay and the retry paths |
| OpenAPI, route, error, and authentication catalogs generated from source definitions | `scripts/generate_public_api.py`, in the gate |

## The decision that matters most

**A federated request is authorized with no established facts.**

A relational fact — owner, same creature, counterparty — is something a node works out
from its own records about its own resources. An envelope cannot establish one by
arriving, however confidently the source asserts it. So the destination passes none, and
a federated request reaches only what a rule allows on `public`, `authenticated`, or an
explicit grant. Anything relational fails closed until the destination resolves it
itself.

The alternative — trusting the source's word for "this subject owns that resource" —
would make every node in the federation as trustworthy as the least careful one.

## Replay and retry are different things

A repeated **nonce** is a replay and is refused. A repeated **request id** is a retry and
is answered from the record without executing anything again. Conflating them would
either break reliability or break security; the live test exercises both and asserts the
execution count stays at one.

## Owned by later phases

- **Serving the contract**: the hardened HTTP stack, its middleware (rate, concurrency,
  body and duration limits, CORS, draining), and the SSE and WebSocket streams.
- **Federation node composition and deployed evidence**: the package now delivers the
  mandatory-mTLS endpoint/client, A401 verification, signed responses, backoff, and
  circuit breaker, plus the A706 trust/partition runbook. The node must supply the
  concrete signer, descriptor verifier, executor, and listener configuration, then the
  rotation/partition drill must be observed between deployed clusters.
- **Retiring the legacy transports** is ADR 0004's window and the removal ledger's.
