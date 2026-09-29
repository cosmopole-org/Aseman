---
status: CURRENT
owner: architecture
source_of_truth: the contracts under contracts/ and their generators
verification: python3 -m unittest discover -s tests/contract-checks -p 'test_*.py'
---

# Contract checks

These suites check the repository's contracts against each other and against the code
that must honor them. `cargo xtask fast` runs them.

- `test_core_boundaries.py`: the configuration schema, the retired-name catalog, and
  the domain and port catalogs.
- `test_module_contracts.py`: the module manifest, lifecycle, placement, trust, and
  admin contracts.
- `test_capsule_contracts.py`: the capsule encoding, kinds, storage classes, guest
  isolation rules, and export framing.
- `test_observability_policy.py`, `test_operational_scenarios.py`,
  `test_release_policy.py`: the operations contracts.
- `test_public_clients.py`: the generated public HTTP clients.

The node's operations are tested in `apps/aseman-node/src/actions/tests.rs`.
