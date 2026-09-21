---
status: DECISION
owner: gateway/migration
source_of_truth: this ADR
last_verified_commit: f6be364d6761
verification: A308 bridge transform tests; P7 gateway conformance
---

# ADR 0024: Legacy bridge grants, topic ownership, and node address hints

## Status

Accepted 2026-09-21. It resolves the P7-owned A308 rows; the P7 gateway consumes the
result.

## Context

The legacy `mintBridgeToken` host call stores a grant at `Json::BridgeGrant::{sha256(token)}`,
path `grant` (`creatureId`, the minting program; `deliverTo`; `routes`; `topics`;
`expiresAt`, where `0` means no expiry; and `createdAt`). It also claims each topic with
`BridgeTopicOwner::{topic} = minter`. Only the token digest is stored. Revocation removes
the grant correctly with `del_json`, but topic claims persist.

`NodeIpToHost::{ip}` is read by federation hostname resolution, but no legacy code
writes it.

## Decision

- Each grant migrates as a `core.bridge_grant` capsule. It holds `token_digest` (the
  32-byte SHA-256; the raw token never existed in storage), `expires_at_micros` (`0`
  keeps "never"), and the complete grant document (ADR 0016). It is owned by the minting
  program's creature. Active bridges keep working without any credential being exposed.
  A digest that is not 64 lowercase hex characters fails closed.
- Each topic claim migrates as `core.bridge_topic` (`topic`, `owner_ref`), owned by the
  claimant's creature. Every topic of a grant must have a claim; orphan claims left by
  revocation migrate, because legacy still enforces them.
- A `NodeIpToHost` record has no reviewed writer. Any such record fails closed, and
  federation addressing is re-established from signed node descriptors (P7-02).
