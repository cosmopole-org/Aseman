---
status: ACCEPTED
owner: migration/P0-06
source_of_truth: scripts/generate_removal_ledger_children.py
last_verified_commit: 800df24076c7
verification: python3 scripts/generate_removal_ledger_children.py --check
---

# Generated removal-ledger child rows

The machine-readable JSON contains owner, callers, target owner, disposition,
expiry, and replacement evidence for every generated child row.

| Class | Rows |
|---|---:|
| Configuration keys | 117 |
| Storage layouts/key families | 172 |
| Protocols, operations, runtimes, CLI, compatibility | 290 |
| Package/artifact owners | 40 |
| **Total** | **619** |

No child row authorizes deletion. The parent ledger's replacement and deletion
gates apply, and rows are retired only with phase-specific evidence.
