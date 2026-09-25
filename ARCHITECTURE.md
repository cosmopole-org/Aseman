# Aseman architecture

Aseman is a ports-and-adapters control plane. Domain rules are pure, application use
cases depend on narrow ports, adapters implement those ports, and executable crates
choose implementations. Independently replaceable providers communicate through the
versioned contracts in [`contracts/`](contracts/).

The authoritative maps are:

- [`plan/migration/01-target-architecture.md`](plan/migration/01-target-architecture.md)
  for the target system and dependency direction.
- [`docs/architecture/state-authority.md`](docs/architecture/state-authority.md) for
  state ownership.
- [`docs/migration/status.md`](docs/migration/status.md) for what is implemented now.
- [`docs/generated/current-workspace.md`](docs/generated/current-workspace.md) for the
  generated package and dependency inventory.
- [`docs/migration/removal-ledger.md`](docs/migration/removal-ledger.md) for legacy
  paths and their deletion gates.

The stable dependency direction is `domain <- ports <- application <- composition`.
Concrete storage, transport, scheduler, runtime, and consensus types may not enter the
domain or application APIs. Run `cargo xtask arch` after changing dependencies and
`cargo xtask fast` before submitting a change.
