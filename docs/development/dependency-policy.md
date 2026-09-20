---
status: ACCEPTED
owner: architecture/phase-1
source_of_truth: Cargo.toml and xtask/src/main.rs
last_verified_commit: 800df24076c7
verification: cargo xtask arch
---

# Dependency policy

The allowed direction is `domain <- ports <- application <- composition/adapters`.
Contracts and configuration are sibling boundary crates; domain never imports them.
Concrete storage, networking, scheduler, runtime, consensus, environment, filesystem,
and process dependencies are forbidden from domain, ports, and application.

`cargo xtask arch` reads Cargo metadata and fails on missing core crates or a forbidden
dependency in the protected layers. New architecture crates inherit workspace lints.
Legacy crates are strangled behind ports and retain their local lint baseline until
their replacement gate; they do not become exceptions for new code.

Dependencies must be version-pinned through the root lockfile, have a clear owner, and
be added at the narrowest layer. Git dependencies require an immutable revision.
Protocol/provider SDK dependencies belong in adapters/modules, never in domain.
