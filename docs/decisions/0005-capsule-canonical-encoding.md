---
status: DECISION
owner: storage/security
source_of_truth: this ADR
last_verified_commit: 800df24076c7
verification: A301 canonical vectors and cross-language conformance
---

# ADR 0005: Deterministic CBOR capsule encoding and SHA-256 integrity

## Status

Accepted 2026-09-19. A301 supplies byte-level vectors before persistence cutover.

## Decision

The portable capsule form is deterministic CBOR using RFC 8949 preferred
serialization rules plus a stricter Aseman profile: definite lengths, shortest integer
and float encodings, map-key ordering by encoded bytes, UTF-8 text, no duplicate keys,
no indefinite items, and rejection of non-finite floats. Timestamps are signed integer
microseconds from the Unix epoch in UTC. Identifiers and digests are byte strings in
CBOR and explicit encodings in JSON views.

The integrity preimage is a versioned, domain-separated deterministic-CBOR structure
that excludes the `integrity_hash` field and transport/provider metadata. The initial
digest is SHA-256 and is identified as `sha2-256`, not inferred from length. Revision
links include the previous digest; tombstones are ordinary signed revisions with no
body. Signatures, where required, cover the domain, encoding version, digest algorithm,
and digest and follow ADR 0009.

JSON is an API/debug projection and is never hashed directly. Unknown encoding or
digest versions fail closed. Providers must round-trip the canonical envelope even
when they use native physical columns.

## Migration and rollback

Legacy values are decoded semantically, normalized once, and stored with both legacy
source checksum and capsule digest in the migration journal. Read-compare uses domain
semantics, not raw legacy bytes. Rollback retains the legacy authority until all
canonical vectors, counts, revisions, and tombstones verify.

Rejected: provider-native row bytes, ordinary JSON serialization, and an
algorithm-without-version field as the integrity contract.
