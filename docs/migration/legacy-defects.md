---
status: CURRENT
owner: migration
source_of_truth: legacy source evidence cited per row
last_verified_commit: f6be364d6761
verification: A308 transform tests encode the observable legacy behavior of each row
---

# Legacy defects found during A308 review

These defects are in the running legacy Caspar node. The migration preserves the
**observable** legacy state and never silently repairs it. Fixing the legacy node is
tracked separately, and security rows should be fixed before cutover.

| ID | Severity | Legacy evidence | Defect | Migration handling |
|---|---|---|---|---|
| LD-01 | Security (critical) | `shell/api/actions/creature.rs` `/creatures/login` | Returns the account's stored RSA private key on every email login; with Firebase disabled, any email is accepted | ADR 0019: keys verified, never exported; RL-019 |
| LD-02 | Security (high) | `creature.rs` `secret_revoke` | Deletes `SecretGrant::…` without the `link::` prefix, so revocation never takes effect and grants stay valid until an unbounded expiry | ADR 0023: grants migrate as legacy enforces them; fix: prefix both deletes with `link::` |
| LD-03 | Security (medium) | `shell/utils/secret_crypto.rs` | Secret ciphertext carries no AAD, so a blob moved to another owner or name still decrypts | ADR 0023: P4 re-wraps with owner/name/epoch AAD before delivery |
| LD-04 | Functional | `creature.rs` `secret_list`, `list_granted_secrets` | Raw `get_by_prefix` never matches `link::` records, so the lists are always empty | Not migrated state; P4 implements listing correctly |
| LD-05 | Functional | `drivers/vmm/driver.rs` guest `dbOp` | `del` removes an unprefixed key (a no-op), and `getByPrefix` scans raw keys (never returns committed pairs) | ADR 0021: committed pairs migrate; the P4-04 gateway fixes both |
| LD-06 | Data retention | `hostcall_entities.rs` store and resource deletes | `Json::StoreMeta`, `Json::VmResourceStore`, and `Json::VmResourceEntity` deletes use unprefixed raw keys, leaving orphaned documents | ADRs 0016/0022: orphans fail closed or migrate as observed by `get` |
| LD-07 | Finance | `creature.rs`/`program.rs` `as_i64` | Float amounts are truncated to integers | ADR 0017: non-integer money fails closed |
| LD-08 | Finance | `program.rs` billing sweep | Stale `VmStatus = running` records keep being billed after a restart, because instances are never relaunched | ADR 0022: P8 reconciles against observed runtime |
| LD-09 | Dead state | `core/core_orchestrator.rs` | `chainCallback::*` is written and never consumed or deleted | ADR 0020: dropped after shape checks |
