# Aseman repository instructions

## Read first

Before changing a capability, read `docs/glossary.md`, `ARCHITECTURE.md`, the accepted
ADRs in `docs/decisions/` that own it, and the contract it is specified by
(`docs/reference/artifacts.md` maps artifact IDs such as `A402` to their files).

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
- Every node operation is one entry in the router's operation table
  (`apps/aseman-node/src/actions/mod.rs`); transports never call a handler directly.

## Change procedure

1. Name the requirement, the owning ADR and contract, and the rollback.
2. Add or update the tests that pin the behavior before changing it.
3. Record a decision that changes a boundary as a new ADR.
4. Run `cargo xtask fast`; run `cargo xtask full` for cross-boundary changes.
5. Regenerate generated contracts and inventories through their generators and review
   the diffs.
