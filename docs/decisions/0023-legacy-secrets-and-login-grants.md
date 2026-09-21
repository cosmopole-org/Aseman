---
status: DECISION
owner: security/migration
source_of_truth: this ADR
last_verified_commit: f6be364d6761
verification: A308 secret/grant transform tests; P4 secret re-wrap before cutover
---

# ADR 0023: Legacy creature secrets and login grants

## Status

Accepted 2026-09-21. It resolves the P4-owned A308 rows without implementing P4
authorization.

## Context

Legacy `/creatures/secretPut` stores `link::Secret::{owner}::{name}` as base64
`nonce(12) || ChaCha20-Poly1305 ciphertext` under a 32-byte node master key read from
`<storage_root>/node-secret-key`. There is **no associated data**, so a ciphertext
copied to another owner or name still decrypts: legacy authorization, not the
encryption, is the boundary.

`SecretGrant::{owner}::{name}::{grantee}` holds an expiry in milliseconds, and
`SecretGrantee::{grantee}::{owner}::{name}` is its reverse index with the same value.
`LoginGrant::{nonce} = {expiresAt}|{email}` is a single-use login bearer nonce with a TTL
of at most 300 seconds.

Plan 05 requires encrypted secrets at rest with short-lived delivery.

## Decision

- Each secret migrates as a `core.creature_secret` capsule owned by its creature. The
  capsule holds the secret `name`, the exact ciphertext, the algorithm label
  `chacha20poly1305-legacy-v1` (nonce prefix, no AAD), and `key_fingerprint`, a
  domain-separated SHA-256 of the legacy master key. Plaintext is never exported,
  logged, or hashed.
- The operator supplies the legacy master key as migration evidence. The export
  authenticates every ciphertext under it (the AEAD tag check) and discards the
  plaintext immediately. A missing key, an undecodable blob, or a failed tag fails
  closed: an unauthenticated ciphertext would be a secret nobody can read.
- Each grant migrates as a `core.secret_grant` capsule (relationships `secret` and
  `grantee`; `expires_at_micros`). Expired grants migrate too, since legacy already
  denies them and P4 enforces expiry. Every `SecretGrantee` reverse link must mirror its
  grant exactly.
- P4 cutover obligation: before a secret is delivered by the target, it must be
  re-wrapped under the target secret provider with the owner, name, and key epoch bound
  as AAD. The legacy master key then leaves service and is retained only for the
  rollback window.
- `LoginGrant` is an ephemeral bearer credential. It is shape-checked and never migrated,
  and a surviving grant is denied rather than honored. Grant-mode login is a P4 identity
  concern, recorded under RL-019.

## Rejected alternatives

- Decrypting at export and re-encrypting in Phase 3: this puts plaintext in the
  migration pipeline before a target secret provider exists.
- Migrating without verification: this could silently carry corrupted or foreign-key
  ciphertext.
