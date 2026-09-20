---
status: CURRENT
owner: storage/application
source_of_truth: ADR 0005 and aseman-contracts::capsule
verification: cargo test -p aseman-contracts capsule::tests
---

# Capsule encoding and revision contract

Version 1 uses RFC 8949 deterministic CBOR with definite lengths, preferred shortest
integer and float widths, text map keys ordered by their encoded bytes, UTF-8 text, and
no duplicate keys, tags, indefinite values, non-finite floats, or trailing bytes.
Identifiers are 16-byte strings; timestamps are signed Unix microseconds. JSON is only
the schema/debug projection and is never an integrity preimage.

The SHA-256 preimage is `ASEMAN-CAPSULE-INTEGRITY-V1\0`, the big-endian encoding
version, `sha2-256`, a zero separator, and the canonical envelope with
`integrity_hash` omitted. Revision one has no previous digest. Every later revision
must name the previous `sha2-256` digest. A tombstone is a normal revision with
`tombstone=true` and no body; a live revision always has a body.

Provider-private columns, keys, and metadata are excluded. Providers must reconstruct
the same canonical envelope and pass the checked byte vector before activation.
