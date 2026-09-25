---
status: CURRENT
owner: documentation
source_of_truth: docs and generated contracts in this repository
verification: cargo xtask fast
---

# Aseman documentation

- [Migration status](migration/status.md) — implemented, partial, and externally
  blocked work.
- [Glossary](glossary.md) — canonical terms and legacy-name mapping.
- [Architecture](architecture/) — trust, state, data flow, consistency, and failure
  models.
- [Decisions](decisions/) — accepted ADRs.
- [Development](development/) — dependency rules and common-change playbooks,
  including the per-runtime [creature implementation guide](development/creature-implementation.md).
- [Operations](operations/) — topology, migration, handoff, and recovery runbooks.
- [Generated references](generated/) — workspace, routes, configuration, contracts,
  mappings, and traceability.
- [Archived Caspar documentation](legacy/caspar/) — explicitly historical material
  retained only for compatibility users; it is not current architecture guidance.

The proposed architecture and its phase gates live in [`../plan/migration/`](../plan/migration/).
Generated documents are not edited by hand.
