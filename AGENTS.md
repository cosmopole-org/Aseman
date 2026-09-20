# Aseman repository instructions

## Read first

Before changing a capability, read `docs/glossary.md`, the owning document in
`plan/migration/`, its accepted ADRs in `docs/decisions/`, and the corresponding row in
`docs/migration/removal-ledger.md` or its generated child ledger.

## Architecture boundaries

- `aseman-domain` is pure: no filesystem, network, database, process, environment,
  runtime, or framework dependencies.
- `aseman-ports` depends only on domain values and defines behavioral requirements.
- `aseman-application` depends on domain and ports; it contains use cases and no driver.
- Contracts own wire values. Config owns environment/file parsing. Executables compose.
- Concrete storage, transport, scheduler, consensus, and runtime types never enter
  domain/application public APIs.
- Guest database/provider/role selection is always resolved server-side from the
  authenticated workload-to-creature binding. Caller-selected tenancy is forbidden.

## Change procedure

1. Name the requirement, artifact, ADR, owner, migration/rollback, and removal row.
2. Add or update characterization before replacing current behavior.
3. Keep compatibility at an edge and give it ADR 0004 expiry evidence.
4. Run `cargo xtask fast`; run `cargo xtask full` for cross-boundary changes.
5. Regenerate inventories intentionally and verify their diffs.

Never delete a legacy path until both its replacement and deletion gates pass.
