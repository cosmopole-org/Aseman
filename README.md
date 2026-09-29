# Aseman

Aseman is a modular workload-hosting control plane. A node hosts *creatures*
(human and machine accounts), their *stores* (shared message spaces), and their
*programs* (deployable workloads). It runs workloads through an out-of-process VMM on
native or Nomad backends, orders shared state and finance on a Hashgraph chain,
federates with other clusters, and keeps its state on a pluggable storage provider
(PostgreSQL or embedded RocksDB).

The code is a Rust workspace with typed domain, port, and application layers, and
versioned contracts between replaceable providers.

## Start here

- [Architecture map](ARCHITECTURE.md): the layers, the node, and the providers.
- [Documentation portal](docs/README.md)
- [Glossary](docs/glossary.md)
- [Contribution workflow](CONTRIBUTING.md)
- [Security policy](SECURITY.md)

## Repository map

- `apps/`: executables. `aseman-node` (the node), `asemanctl` (administration and
  bootstrap), `aseman-vmm` (the VMM service), `aseman-vmm-agent` (the privileged
  Firecracker worker agent), `aseman-meter` (metering), `aseman-keygen`, and
  `aseman-client` (command-line and generated HTTP clients).
- `crates/`: the domain, ports, application use cases, contracts, configuration, the
  storage module and its provider loader, and shared runtime libraries.
- `modules/`: providers and transports: storage (PostgreSQL, RocksDB), consensus
  (Hashgraph), VMM backends (native, Nomad) and their runtime plugins, identity,
  policy, federation, the public HTTP service, and the network transports.
- `contracts/`: source schemas and wire contracts; the public HTTP API is
  `contracts/public/openapi.json`, the action registry `contracts/security/actions.json`.
- `deploy/`: container images, compose profiles, systemd units, and observability
  assets.
- `docs/`: architecture, decisions (ADRs), development guides, operations runbooks,
  references, and generated catalogs.
- `tests/`: conformance kits, contract checks, the storage-migration end-to-end suite,
  and the agent-comprehension evaluations.
- `xtask/`: architecture and verification automation.

The generated [repository layout](docs/generated/repository-layout.md) and
[workspace inventory](docs/generated/current-workspace.md) list every package.

## Development

The pinned toolchain and the root workspace are authoritative:

```sh
cargo xtask arch   # dependency direction
cargo xtask fast   # formatting, generated contracts, contract checks, core tests, lints
cargo xtask full   # fast, plus the node's binaries and the native VMM backend's runtimes
```

Live suites use the configured PostgreSQL, Nomad, Docker, and Firecracker services
(`ASEMAN_TEST_POSTGRES_URL`, …); a skipped external-service test is not proof that the
corresponding deployment works.

Build the executables from their application roots:

```sh
cargo build -p aseman-node
cargo build -p asemanctl
cargo build -p aseman-vmm
```

`asemanctl bootstrap` brings up a compact deployment; see
[docs/operations/topology.md](docs/operations/topology.md).
