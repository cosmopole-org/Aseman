---
status: ACCEPTED
owner: federation/network
source_of_truth: modules/federation/http, contracts/federation/envelope-v1.md, contracts/security/identity-v1.md
verification: cargo test -p aseman-federation-http
---

# Federation HTTP transport v1

`POST /v1/federation/envelopes` accepts JSON containing the A705 `envelope` and an
unpadded-base64url `payload_base64`. `Aseman-Proof` is an A401 request proof by the
source node. Its request ID, action, resource, subject node, audience, and body digest
must bind the envelope and decoded payload exactly.

The destination verifies A401 before A705 replay checks and destination authorization.
The response states `executed`, `replayed`, or `refused`; it carries the request ID and
is signed by the destination over the canonical unsigned response fields. An HTTP peer
must additionally authenticate at the mandatory mTLS boundary. The server trusts only
the configured federation CA; the client installs only the configured peer roots and
presents its node certificate. Body limits are enforced before parsing, and malformed
or unsigned requests never reach application code.

The outbound client retries only transport failures, `429`, `502`, `503`, and `504`,
using bounded exponential backoff. Every retry reuses the exact envelope, request ID,
nonce, payload, and proof, so the destination replays a durable answer rather than
repeating an effect. Consecutive unavailable or invalid signed responses open a bounded
circuit. Non-retryable destination refusals close/reset the failure run. A response is
accepted only when its request ID and outcome are valid and its signature verifies
against the destination descriptor.
