---
status: CURRENT
owner: module-platform
source_of_truth: contracts/module/placement.schema.json and crates/aseman-module-runtime
verification: cargo test -p aseman-module-runtime cluster_routing_advances_only_after_quorum
---

# Cluster module placement and reconciliation

Cluster desired state names an exact module key, artifact digest, required host set,
quorum, and routing generation. Every host independently verifies the signature and
digest before reporting `verified`, then stages and health-checks before `ready`.

Reconciliation installs a missing or unverified placement and stages a verified but
unready placement. It may advance routing only after the configured quorum is ready.
Stale observed generations never overwrite desired state. Shrinking placement,
changing quorum, or deleting an artifact is a separate authorized desired-state
operation and is not inferred from host disappearance.
