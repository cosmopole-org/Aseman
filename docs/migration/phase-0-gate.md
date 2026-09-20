---
status: ACCEPTED
owner: migration
source_of_truth: plan/migration/09-migration-phases.md
last_verified_commit: 800df24076c7
verification: commands below
---

# Phase 0 exit gate

## Decision

Accepted 2026-09-19. Phase 1 boundary work may begin. This gate accepts current-truth
artifacts and target decisions; it does not waive any phase-specific replacement,
security, performance, migration, rollback, or deletion criterion.

## Evidence

- A001–A007 inventories are reproducible and feed the current call path, support
  manifest, and removal ledger.
- A008 assigns every observed shell action, HTTP route, guest operation, runtime, CLI
  command, and script a current owner, target owner, disposition, expiry, and executable
  golden evidence. Deeper behavior is required before its owning rewrite/deletion.
- A009 records passing node/CLI correctness and resource measurements, static debt
  ratchets, and explicit first measurable service/recovery targets.
- A010 establishes state authority, trust boundaries, data flows, failure behavior, and
  the threat model, including per-creature database/role isolation.
- A011 contains the parent ledger and 560 generated child rows.
- ADRs 0001–0014 resolve the complete blocking queue; ADR 0013 is A014.
- A013 defines canonical naming and the bounded compatibility mapping.

## Verification commands

```bash
PYTHONDONTWRITEBYTECODE=1 python3 scripts/generate_current_workspace_inventory.py --check
PYTHONDONTWRITEBYTECODE=1 python3 scripts/generate_current_surface_inventories.py --check
PYTHONDONTWRITEBYTECODE=1 python3 scripts/generate_legacy_data_inventory.py --check
PYTHONDONTWRITEBYTECODE=1 python3 scripts/generate_current_call_graph.py --check
PYTHONDONTWRITEBYTECODE=1 python3 scripts/generate_characterization_fixtures.py --check
PYTHONDONTWRITEBYTECODE=1 python3 scripts/generate_support_manifest.py --check
PYTHONDONTWRITEBYTECODE=1 python3 scripts/generate_quality_baseline.py --check
PYTHONDONTWRITEBYTECODE=1 python3 scripts/generate_removal_ledger_children.py --check
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests/characterization -p 'test_*.py'
cargo test --manifest-path node/Cargo.toml --workspace
cargo test --manifest-path cmd/casparctl/Cargo.toml
```

The TypeScript client typecheck remains explicitly unavailable until dependencies are
installed; Phase 1 must make it hermetic before changing that client. Operational
latency/recovery numbers remain phase-owned baseline targets, not current claims.
