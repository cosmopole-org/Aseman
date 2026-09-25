# Contributing to Aseman

Read [`AGENTS.md`](AGENTS.md), the relevant migration design, accepted ADRs, and the
matching removal-ledger row before changing a capability. Current migration state and
the recommended reading order are in
[`docs/migration/status.md`](docs/migration/status.md).

Use the root workspace and pinned toolchain:

```sh
cargo xtask doctor
cargo xtask fast
```

Run `cargo xtask full` for changes that cross a process, storage, security, network,
VMM, or compatibility boundary. Generated files identify their generator; update them
through that generator and verify the resulting diff. Never delete a legacy path until
its replacement and deletion gates both pass.

Every migration change records its requirement, work package, accepted ADRs, state
owner, migration and rollback, tests, generated artifacts, and removal-ledger rows.
The template is in
[`plan/migration/15-agent-execution-guide.md`](plan/migration/15-agent-execution-guide.md).
