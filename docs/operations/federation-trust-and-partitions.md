---
status: CURRENT
owner: federation/operations
source_of_truth: ADR 0009, contracts/federation/http-v1.md, contracts/federation/directory/descriptors-v1.md
verification: cargo test -p aseman-federation-http
---

# Federation trust, rotation, revocation, and partitions

Federation trust is administrator-enrolled. Production does not use unattended trust
on first use. Enrolling a peer pins its federation root, stable node ID, current key
epoch, and expected HTTPS endpoint. The TLS root and the descriptor-signing root are
checked independently; passing one boundary never substitutes for the other.

## Bootstrap a peer

1. Obtain the peer root, node ID, initial signed descriptor, and fingerprint through an
   authenticated administrative channel.
2. Compare the out-of-band fingerprint before writing either root.
3. Validate the descriptor signature, node-ID binding, endpoint, expiry, protocol
   version, and that its current epoch is not revoked.
4. Install the TLS root and descriptor root atomically, then record the descriptor.
5. Send a read-only diagnostic envelope. Confirm mTLS identity, A401 proof validation,
   destination authorization, and response-signature validation in the correlated
   audit records before permitting mutations.

A signed introduction may replace steps 1–2 only when its signer is already an
enrolled root and the introduction delegates no broader trust than that root.

## Rotate a node key

Publish the higher descriptor sequence with the new epoch before using it. Keep only
the current and immediately prior epoch during the bounded overlap. Observe successful
traffic on the new epoch, then publish another higher sequence that ends overlap.
Rollback may restore the prior epoch only while it remains in overlap and has not been
revoked. A lower descriptor sequence is never a rollback mechanism.

## Revoke a peer or epoch

Publish and distribute a higher-sequence descriptor carrying the revoked epoch, or
remove the peer root when the whole peer is distrusted. Revocation overrides overlap.
Do not erase replay records or cached answers: they prevent captured traffic from
becoming executable again. Reject a descriptor that revokes its own active epoch.

## During a partition

Do not discover new trust or extend descriptor expiry merely to restore connectivity.
Continue serving only descriptors that are still valid. Outbound calls use bounded
backoff and the circuit breaker; callers receive an unavailable result after the
deadline and retain their request ID for an explicit retry. Never route around a
partition through an untrusted node. A hop limit decreases on every trusted relay and
ends a loop at zero.

## Recovery

After connectivity returns, fetch descriptors through an enrolled peer, accept only
higher sequences/revisions, apply revocation before ordinary traffic, and then close
the circuit with a signed diagnostic response. Retrying a previously submitted request
uses the same request ID and receives the durable recorded answer. Operators compare
audit streams for unauthorized acceptances, duplicate effects, stale descriptors, and
signature failures before declaring recovery.
