---
status: ACCEPTED
owner: security/identity
source_of_truth: this contract, ADR 0009
last_verified_commit: 5c6e6eb
verification: cargo test -p aseman-contracts identity (regenerate vectors with `-- --ignored`); cargo test -p aseman-domain identity
---

# A401: identity, key, signature, and proof formats (v1)

This contract fixes the exact encodings that ADR 0009 decides in principle. The Rust
reference implementation is `aseman-contracts::identity` (wire formats) and
`aseman-domain::identity` (verification rules). The vectors in
`vectors/identity-v1.json` are generated from that code by its tests and must match
byte for byte. Any other implementation must reproduce them.

## 1. Subjects

An identity is a class plus an opaque UUID. A bare UUID is never authority.

| Class | Text | Meaning |
|---|---|---|
| `user` | `user:{uuid}` | A human account |
| `creature` | `creature:{uuid}` | A non-human principal that owns programs and data |
| `node` | `node:{uuid}` | A federated Aseman node (one identity for all its replicas) |
| `service` | `service:{uuid}` | An internal service of a node |
| `workload` | `workload:{uuid}` | One workload, bound to its home node |
| `module_publisher` | `module_publisher:{uuid}` | A module signer (P2 trust roots) |

The canonical text is `{class}:{uuid}`, with the UUID in lowercase hyphenated form. It
is the only accepted spelling: uppercase, braces, URNs, and simple (unhyphenated)
forms are `malformed`. New identities use UUIDv7. Identities migrated from legacy keep
their deterministic capsule IDs, and `core.legacy_identity` maps them.

## 2. Public keys

**Encoding:** `version || multicodec || key`.

- `version` is the byte `0x01`.
- `multicodec` is the key type as an unsigned varint:

| Algorithm | Multicodec | Varint bytes | Key bytes | Status |
|---|---|---|---|---|
| Ed25519 | `ed25519-pub` 0xed | `ed 01` | 32-byte public key | Canonical: the only algorithm that issues identities, tokens, descriptors |
| RSA | `rsa-pub` 0x1205 | `85 24` | SPKI DER | Legacy creature keys: verification only |
| secp256k1 | `secp256k1-pub` 0xe7 | `e7 01` | 33-byte compressed point | Legacy consensus peer keys: verification owned by P8, `unsupported_algorithm` until then |

**Text:** multibase base58btc, that is `z` followed by the base58btc (Bitcoin alphabet)
of the encoding. A text must round-trip exactly, so every value has one spelling.

**Fingerprint:** SHA-256 of the complete versioned encoding.

**Key ID:** the multibase base58btc text of the multihash `12 20 || fingerprint`
(sha2-256, 32 bytes). Key IDs always start with `zQm`.

## 3. Signed bytes

A signature covers exactly:

```text
"aseman-signature-v1" 0x00 {context} 0x00
field_1 ... field_13
```

Each field is a 4-byte big-endian length followed by its bytes. The fields are, in
this order:

| # | Field | Bytes |
|---|---|---|
| 1 | algorithm | UTF-8: `ed25519` or `rsa-pss-sha256` |
| 2 | key ID | UTF-8 (section 2) |
| 3 | key epoch | `u32` big-endian |
| 4 | subject | UTF-8 canonical text (section 1) |
| 5 | audience | UTF-8, the verifier's exact audience string |
| 6 | issued at | `i64` big-endian Unix milliseconds |
| 7 | not before | `i64` big-endian Unix milliseconds |
| 8 | expires at | `i64` big-endian Unix milliseconds |
| 9 | nonce | 16 to 64 random bytes |
| 10 | request ID | UTF-8 |
| 11 | action | UTF-8, the registered action (A402) or the HTTP method and route |
| 12 | resource | UTF-8, possibly empty |
| 13 | body digest | 32 bytes, SHA-256 of the exact request body (empty body included) |

The context names what is being signed. A signature never verifies in another context:

| Context | Signed by key purpose | Use |
|---|---|---|
| `request` | `authentication` | A subject's request (guest API, node API) |
| `challenge` | `authentication` | Answer to a server challenge. The nonce is the server's, and one use consumes it |
| `token` | `token_issuing` | Capability tokens (claims digest in the body digest, A403) |
| `descriptor` | `descriptor` | Node descriptors (P7-02) |
| `revocation` | `descriptor` | Revocation statements (section 7) |
| `introduction` | `introduction` | A federation root introducing a node (section 8) |

Ed25519 signs the bytes directly (RFC 8032). A legacy RSA key uses RSASSA-PSS with
SHA-256 (MGF1-SHA-256, salt length 32) over the same bytes.

## 4. Signed-request proof

The proof travels as a JSON object with exactly these members. Unknown members are
rejected.

| Member | Type |
|---|---|
| `version` | `1` |
| `context` | one of the section 3 contexts |
| `algorithm`, `key_id`, `subject`, `audience`, `request_id`, `action`, `resource` | strings as in section 3 |
| `key_epoch` | integer |
| `issued_at_millis`, `not_before_millis`, `expires_at_millis` | integers |
| `nonce`, `body_digest`, `signature` | unpadded base64url (RFC 4648 section 5) |

Size limits: audience, request ID, and action are 1 to 1024 bytes, and the resource is
at most 4096 bytes.

## 5. Validation order

A verifier applies these checks in order and reports the first failure's code. Cheap
checks come first. The nonce is recorded only after the signature verifies, so an
unsigned request can never burn another request's nonce.

1. `version` is 1, else `unsupported_version`.
2. `algorithm` is known, else `unsupported_algorithm`.
3. Encodings, lengths, subject text, and key ID are well formed, else `malformed`.
4. The key directory has `key_id`, else `unknown_key`.
5. The key belongs to `subject`, else `key_subject_mismatch`. Its recorded epoch equals
   `key_epoch`, else `epoch_not_accepted`.
6. The key's purpose signs `context`, else `key_purpose_mismatch`.
7. The key epoch is accepted (section 6): `revoked_key`, `legacy_key_cannot_sign`,
   `not_yet_valid`, `expired`, or `epoch_not_accepted`.
8. `audience` equals the verifier's audience, else `audience_mismatch`.
9. The time window is fresh (below): `malformed`, `lifetime_too_long`,
   `not_yet_valid`, or `expired`.
10. The body digest equals SHA-256 of the received body, else `body_digest_mismatch`.
11. The signature verifies, else `bad_signature`.
12. The nonce is recorded atomically under (key ID, nonce), else `replayed`.

**Freshness.** Let `skew` be the tolerated clock skew and `max` the maximum lifetime of
the credential's purpose. The window is accepted when all of these hold:
- `issued_at <= not_before < expires_at`
- `expires_at - issued_at <= max`
- `not_before <= now + skew`
- `now < expires_at + skew`

Defaults are a 30-second skew, and for guest (workload) credentials a 5-minute
maximum, the ADR 0009 ceiling.

**Replay.** The verifier remembers each (key ID, nonce) until `expires_at + skew`. A
remembered pair is rejected. A challenge nonce is issued by the server, stored with
its audience and expiry, and consumed by its first valid answer.

## 6. Key epochs and rotation

Keys are recorded per subject and purpose with a strictly increasing `u32` epoch
starting at 1. A legacy verification key is epoch 0. An epoch record holds:
- the key ID and the versioned encoding
- `not_before` and an optional `expires_at`
- `retired_at`, set when the next epoch becomes current
- `revoked_at`
- the legacy flag

An epoch verifies at `now` when all of these hold:
- It is not revoked. Revocation overrides every overlap.
- It is not legacy, or the context does not issue authority (`token`, `descriptor`,
  `revocation`, `introduction`).
- It is within `not_before` and `expires_at`.
- Either it is the current epoch (the highest valid, unrevoked one), or it is the
  immediately prior epoch and `now < retired_at + overlap`.

The default overlap is 24 hours.

## 7. Revocation

A revocation names a key ID and epoch, the subject, `revoked_at`, and a reason
(`compromised`, `superseded`, `decommissioned`). It is stored with the key record
(`revoked_at`) and takes effect at once on the node that records it. Other nodes learn
of it through a revocation statement signed in the `revocation` context by the node's
current `descriptor` key. It carries the statement fields in the body digest, and the
fan-out is delivered in P7. Rollback never restores a revoked key.

## 8. Trust roots

There is no global PKI and no trust on first use in unattended production.

- **Enrollment.** An administrator enrolls a federation root by registering the root
  node's `introduction`-purpose key. It is the ordinary key registration of section 6,
  authorized as an administrative action (A402) and recorded in the audit trail.
- **Introduction.** A root introduces a node by signing an `introduction` proof whose
  body is the canonical introduction record:

  ```text
  "aseman-introduction-v1" 0x00
  len-prefixed: subject text, key encoding, u32 epoch, i64 not_before,
                expires (empty when absent, else i64)
  ```

  The verifier checks the proof in the section 5 order and registers the introduced
  node's key for that epoch.
- **No chaining.** An introduction registers only a `descriptor` key, and only nodes
  introduce nodes. An introduced node therefore never holds an `introduction` key, and
  trust never extends beyond the enrolled roots.

## 9. Key records

Every identity key, including node keys, is one `core.identity_key` record with these
fields:
- subject class and ID
- purpose
- epoch
- key ID (unique)
- versioned encoding
- validity window
- `retired_at`
- `revoked_at`
- legacy flag

It is unique per (subject class, subject ID, purpose, epoch). `core.node_key`, which
had no writer, is replaced by it.

## 10. Legacy keys

A legacy creature's RSA key is registered as its epoch-0 legacy `authentication` key
only after proof of possession: a `challenge` signed with that key. Its subject is the
creature's typed identity, and `core.legacy_identity` maps it. It verifies requests and
challenges during the ADR 0004 compatibility window. It never signs authority-issuing
contexts, and it is retired when the creature registers an Ed25519 key.

Legacy packets that clients sign today (the raw-packet RSA-PSS/PKCS#1 v1.5 scheme in
`ISecurity::auth_with_signature`) stay a transport-edge adapter until P7-05 expires it.
They do not use this structure.
