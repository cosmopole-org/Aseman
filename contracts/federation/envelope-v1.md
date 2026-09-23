---
status: ACCEPTED
owner: federation
source_of_truth: this contract, aseman-domain::federation, plan/migration/06-network-federation-realtime.md
last_verified_commit: eebb9c5
verification: cargo test -p aseman-domain federation
---

# A705: the federation envelope (v1)

An envelope carries one cross-node request between two independently administered
Aseman clusters. Nomad federation is not this: Nomad connects infrastructure, Aseman
federation connects node-clusters and authorizes application-level operations.

## The rule everything else serves

**Resolving an identity never grants authority.** Any authenticated workload in the
federation may resolve another's minimal descriptor. Every lifecycle, terminal,
signalling, data, network, or delegation action is still authorized *at the
destination, by the destination, against its own records* — whatever the source
believed when it sent the envelope.

## Fields

| Field | What it is |
|---|---|
| `request_id` | The identity of this request. A retry carries the same one and is answered from the record rather than executed again |
| `source_node`, `destination_node` | Node IDs. They are never the same node |
| `subject` | Who is asking, as the source authenticated them |
| `target` | What is being acted on |
| `action` | The registered A402 action |
| `payload_digest` | `sha256:{hex}`, so the signature covers the body without carrying it |
| `issued_at_millis`, `expires_at_millis` | Its window, checked against the destination's clock |
| `nonce` | Unique per source node; the destination remembers it until expiry |
| `hop_limit` | How many more nodes may forward it |
| `version` | `1`. An unknown version is refused, never guessed |

## What the destination checks, before any authorization

1. The version is one it speaks.
2. The envelope is addressed to **it**, not merely handed to it.
3. Source and destination are different nodes.
4. It has not expired, by the destination's own clock.
5. Its lifetime is at most **60 seconds**. A long-lived envelope is a replay waiting to
   happen, and the destination must remember every nonce until expiry.
6. Its hop limit is at most **4**.
7. The nonce is present, bounded, and not already seen. A repeated nonce is a replay.
8. The payload digest really is a SHA-256 digest.
9. It names a subject, a target, and an action.

Only then is the subject reauthorized and the action decided. A failure at any step is
the destination's refusal, with a stable reason.

## Hops

Forwarding decreases the hop limit and refuses at zero, so a routing loop ends after at
most four hops even if a node misbehaves.

## Retries

Retries use exponential backoff and a circuit breaker. A retry carries the same
`request_id`; the destination deduplicates on it and returns the recorded signed
response or operation ID rather than executing the request twice.

## Refusals

`unknown envelope version`, `this envelope is addressed to another node`,
`an envelope's source and destination are different nodes`, `the envelope has expired`,
`an envelope may not be valid for that long`,
`the hop limit is above the federation maximum`,
`the envelope may not be forwarded again`, `the nonce is missing or too long`,
`this envelope has already been seen`, `the payload digest is not a sha256 digest`,
`the envelope names no subject, target, or action`.
