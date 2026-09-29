---
status: ACCEPTED
owner: migration/P0-02
source_of_truth: legacy behavior plus source-derived A002-A007 inventories
verification: python3 -m unittest discover -s tests/characterization -p 'test_*.py'
---

# Legacy behavior characterization

This directory is artifact A008. `surface-golden.json` freezes the currently
observed interface names, action dispatch paths, runtime capability declarations,
CLI dispatch entries, and physical persistence shapes. Its test fails when those
surfaces drift without an intentional fixture update.

`support-manifest.json` records current and target ownership, disposition, expiry,
and executable characterization evidence for every public/guest/runtime/CLI row.
Compatibility support is not approval of legacy security semantics; every replacement
must pass the deeper phase-specific contract and adversarial gates. The golden omits
source line numbers so harmless file movement does not change behavioral values.

## Covered in this slice

- signed shell action paths and request types;
- HTTP route method/path/surface tuples;
- guest host-operation aliases;
- action handler, guard, direct storage/service/model/VMM evidence;
- runtime keys, aliases, capabilities, and operation overrides;
- `casparctl`, client CLI, and root-script dispatch entries;
- application, Hashgraph, QuestDB, and OpenRaft persistence shapes.
- custom VM HTTP route normalization, key encoding, target compatibility, and
  invalid-target rejection (`aseman_contracts::legacy_gateway::tests`).
- public storage HTTP identifier validation, header sanitization, JSON escaping
  (`aseman_contracts::legacy_storage_http::tests`), and adapter response framing
  (`storage_http::characterization_tests`).

## Required before each owning replacement gate

- representative request/response and error golden payloads;
- authentication, authorization, and cross-creature isolation behavior;
- state-transition and transaction commit/rollback behavior;
- federation and transport framing compatibility;
- runtime lifecycle, HTTP forwarding, and failure behavior;
- CLI exit codes, stdout/stderr, and filesystem/process side effects;
- machine-readable response/error, state-transition, failure, and performance cases
  defined by the owning phase contract.

Regenerate after an intentional inventory change with:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 scripts/generate_characterization_fixtures.py
```

Run the behavior-level ingress cases with:

```sh
cargo test -p aseman-contracts
cargo test -p aseman-node --lib storage_http::characterization_tests
```
