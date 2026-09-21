---
status: DECISION
owner: security/migration
source_of_truth: this ADR
last_verified_commit: f6be364d6761
verification: A308 custody verification tests; ADR 0009 proof-of-possession at cutover
---

# ADR 0019: Legacy custodial private keys are verified and never migrated

## Status

Accepted 2026-09-21. It applies ADR 0009 to the legacy `UserPrivateKey` link family.

## Context

Legacy `/creatures/login` creates a human creature with a server-generated RSA-2048 key
pair and stores the PKCS#8 private key in plaintext at `link::UserPrivateKey::{id}`. The
action then returns that private key on every successful email login. When Firebase
verification is disabled (the documented DEV path), any email is accepted, so any caller
can obtain the private key of any account registered with that email. The key material
is in the replicated legacy store.

ADR 0009 allows RSA only as a legacy verification adapter. It registers a legacy key as a
verification epoch only after proof of possession and issues new identities with
canonical Ed25519 keys.

## Decision

- Private key material is never exported, imported, logged, or placed in any capsule,
  digest input, or error message. The export has no target kind for it.
- For each `UserPrivateKey::{id}`, the export requires a legacy creature `{id}`, parses
  the value as a PKCS#8 RSA private key, derives its public key, and compares it with the
  creature's registered public key. A malformed key, a missing creature, or a mismatch
  fails closed: legacy login would hand out a key that does not match the identity.
- The export reports only the number of verified custodial keys. The transform manifest
  records the family as `intentional_removal`.
- Cutover prerequisite (P4-01): before the legacy store stops being authoritative,
  affected users must prove possession of the legacy key and enroll a canonical key under
  ADR 0009, or the operator must accept that those legacy RSA identities become
  verification-only. Custodial keys are never re-issued by Aseman.
- The legacy source stays immutable until its deletion gate, so rollback needs no key
  material from Aseman.

## Security note

The custodial login path is a legacy vulnerability, especially with verification
disabled. It is recorded for the P4 identity work and the removal ledger rather than
patched in the legacy node by this migration change.

## Rejected alternatives

- Migrating keys encrypted under an Aseman-managed key: this perpetuates server-side
  custody of user identity keys, which ADR 0009 rejects.
- Silently dropping the family: this would hide a custody relationship that cutover
  must resolve explicitly.
