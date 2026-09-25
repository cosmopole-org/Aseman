# Aseman

Aseman is a modular workload-hosting control plane under active migration from the
legacy Caspar implementation. The checked-in revision uses a root Rust workspace,
typed domain/application/port boundaries, versioned contracts, PostgreSQL adapters,
an out-of-process VMM, native and Nomad backends, capability policy, federation,
durable realtime, and finance/metering rules.

Migration is not complete. PostgreSQL cutover is an operator action, the public HTTP
service is not yet composed into the node, Phase 9 packaging is partial, and legacy
paths remain until their replacement, rollback, and ADR 0004 deletion gates pass. See
the [current migration status](docs/migration/status.md) for the exact state and the
[generated requirements report](docs/generated/requirements-traceability.md) for
traceability.

## Start here

- [Architecture map](ARCHITECTURE.md)
- [Documentation portal](docs/README.md)
- [Canonical glossary](docs/glossary.md)
- [Current workspace inventory](docs/generated/current-workspace.md)
- [Migration plan](plan/migration/README.md)
- [Contribution workflow](CONTRIBUTING.md)
- [Security policy](SECURITY.md)

Legacy Caspar behavior is isolated at canonical edges: warning alias binaries live in
the Aseman app packages, the legacy client lives at `apps/aseman-client`, runtime
compatibility code is under `modules/runtime`, and historical operator material is
archived under `docs/legacy/caspar`. The former `wiki`, `node`, `cmd`, `client-cli`,
`sdk`, `vm-sdk`, and `vms` roots have been consolidated. Remaining compatibility
conditions are tracked in the [removal ledger](docs/migration/removal-ledger.md).

## Repository map

- `apps/` — executable composition roots (`aseman-node`, `asemanctl`, VMM, and agent).
- `crates/` — reusable domain, ports, application, contracts, configuration, and
  runtime libraries.
- `modules/` — provider and transport implementations.
- `contracts/` — source schemas and wire contracts.
- `deploy/` — deployment-profile ownership root; executable assets are still Phase 9.
- `docs/` — current architecture, ADRs, development guidance, operations, and
  generated references.
- `examples/` — public-contract examples as they are delivered.
- `tests/` — characterization, conformance, migration, and cross-service suites.
- `evals/agent/` — cold-start repository-comprehension evaluations.
- `xtask/` — deterministic architecture and verification automation.

The generated [repository hierarchy report](docs/generated/repository-layout.md)
compares the current tree with the plan’s canonical final hierarchy without treating
an empty directory or compatibility wrapper as completed implementation.

## Development

The pinned toolchain and root workspace are authoritative:

```sh
cargo xtask doctor
cargo xtask fast
```

Run `cargo xtask full` for changes that cross storage, security, process, network,
VMM, finance, or compatibility boundaries. Live suites use the configured PostgreSQL,
Nomad, Docker, and Firecracker services; a skipped external-service test is not proof
that the corresponding deployment gate passed.

Canonical binaries are built from their application roots:

```sh
cargo build -p aseman-node
cargo build -p asemanctl
cargo build -p aseman-vmm
```

`caspar-node` and `casparctl` are deprecated compatibility aliases governed by ADR
0004. No stable replacement release or compatibility-window start is implied merely
by their presence in this development tree.
