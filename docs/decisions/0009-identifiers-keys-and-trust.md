---
status: DECISION
owner: security/federation
source_of_truth: this ADR
last_verified_commit: 800df24076c7
verification: A401 vectors, rotation/revocation tests, federation conformance
---

# ADR 0009: Typed identifiers, Ed25519 identities, and explicit trust roots

## Status

Accepted 2026-09-19. Exact binary/JSON vectors are delivered by A401 before key
issuance or federation cutover.

## Decision

Entity IDs are typed, opaque 128-bit UUIDv7 values rendered as lowercase canonical
UUID strings with a context field/type, never as authority. Cryptographic identities
use Ed25519 initially. Public keys are encoded as versioned multicodec bytes and
rendered with multibase base58btc; fingerprints are the SHA-256 digest of the complete
versioned key encoding. Algorithms and encodings are explicit, not inferred.

Every user, node, service, module publisher, and workload has a distinct key purpose.
Signatures cover a deterministic, domain-separated structure containing protocol
version, algorithm, key ID/epoch, subject, audience, issued/not-before/expires times,
nonce, request ID, method/action, resource, and SHA-256 body digest. The initial
signature algorithm is Ed25519; RSA and secp256k1 remain legacy-verification adapters
only and cannot issue new canonical identities.

Node trust is explicit: an administrator enrolls/pins a federation root or accepts a
signed introduction from an already trusted root. There is no global implicit PKI and
no trust-on-first-use in unattended production. Descriptors are signed by the node key,
carry endpoint set, key epoch, expiry, and revocation references, and cannot delegate
broader trust than their enrolled root.

Workload keys are generated/registered for one workload identity and home node. Guest
requests use a maximum five-minute credential lifetime and a server-configured replay
window; one-time challenges/nonces cannot be reused. Rotation permits a bounded overlap
of current and immediately prior epochs; revocation overrides overlap.

## Migration and rollback

Legacy keys are registered as legacy verification epochs after proof of possession and
mapped to new typed IDs. New keys are issued in the canonical format. During the
compatibility window both verify, but only canonical keys sign new descriptors/tokens.
Rollback restores the prior key epoch, never a revoked key, and preserves replay state.

Rejected: identity derived from network address, bare UUID authority, unversioned PEM
as a public protocol, global TOFU, and caller-selected creature/database identity.
