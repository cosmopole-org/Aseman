---
status: CURRENT
owner: documentation
source_of_truth: docs and generated contracts in this repository
verification: cargo xtask fast
---

# Aseman documentation

- [Glossary](glossary.md): canonical terms and the Caspar names.
- [Architecture](architecture/): trust, state, data flow, consistency, and failure
  models. The top-level [architecture map](../ARCHITECTURE.md) is the entry point.
- [Decisions](decisions/): accepted ADRs. [ADR 0039](decisions/0039-the-node-runs-on-one-router.md)
  records the node's router and the close of the Caspar-to-Aseman migration.
- [Development](development/): dependency rules and common-change playbooks,
  including the per-runtime [creature implementation guide](development/creature-implementation.md).
- [Operations](operations/): topology, storage providers, backup and restore,
  upgrades from Caspar-era deployments, and recovery runbooks;
  [storage providers](operations/storage-providers.md) covers PostgreSQL sharding and
  RocksDB/OpenRaft clusters.
- [Reference](reference/): the [artifact catalog](reference/artifacts.md) (`A###`) and
  the [defects the rewrite resolved](reference/defects.md) (`LD-##`).
- [Generated references](generated/): workspace, routes, configuration, contracts,
  and mappings. Generated documents are not edited by hand.
