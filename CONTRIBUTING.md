# Contributing to Aseman

Read [`AGENTS.md`](AGENTS.md), [`ARCHITECTURE.md`](ARCHITECTURE.md), and the accepted
ADRs that own the capability before changing it.

Use the root workspace and pinned toolchain:

```sh
cargo xtask fast
```

Run `cargo xtask full` for changes that cross a process, storage, security, network,
or VMM boundary. Generated files name their generator; update them through it and
review the resulting diff.

A change records its requirement, the ADRs and contracts it follows or amends, the
state it owns, its rollback, and its tests.
