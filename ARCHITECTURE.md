# Aseman architecture

Aseman is a ports-and-adapters control plane. Domain rules are pure, application use
cases depend on narrow ports, adapters implement those ports, and executables choose
the implementations. Independently replaceable providers communicate through the
versioned contracts in [`contracts/`](contracts/).

## Layers

The dependency direction is `domain <- ports <- application <- composition`:

- `aseman-domain`: values and rules, with no I/O.
- `aseman-ports`: the behavioral requirements the use cases need, with conformance
  suites every adapter passes.
- `aseman-application`: the use cases (creatures, stores, programs, finance, identity,
  guest calls, federation, the public action service, VMM control).
- `aseman-contracts` owns wire values, `aseman-config` owns environment and file
  parsing, and the executables in `apps/` compose.

Concrete storage, transport, scheduler, runtime, and consensus types never enter the
domain or application APIs; `cargo xtask arch` enforces the direction.

## The node

`apps/aseman-node` runs every operation through one router (ADR 0039):

- `actions/`: the operation table, checked against the A402 action registry, and the
  operation families. An operation is one storage transaction, authorized and
  admitted by its packet guard.
- `transports/`: the public HTTP API (A701), public storage HTTP, module admin, and the
  signed-packet transports (TCP, WebSocket, federation, the chain), which are framing
  only.
- `live/`: the signal hub and durable topics.
- `workloads/`: the VMM client, the guest host calls, and HTTP ingress to workloads.
- `state/`: the node's models and the ports over them.
- `node/`: composition and the accessors of the node's components.

## Storage

The storage module (`aseman-storage`, ADR 0036) is the one door to the database. It
loads the configured provider plugin (PostgreSQL or RocksDB, ADR 0033/0034), and every
persisted port is an adapter on it (ADR 0038). Each creature's guest data lives in its
own database, resolved server-side from the authenticated workload (ADR 0001).

## Workloads

The node commands an out-of-process VMM (`aseman-vmm`, A501). The VMM runs workloads on
a backend: `native` (the runtime plugins in `modules/runtime/`) or Nomad, with
Firecracker microVMs through the privileged worker agent (ADR 0010, ADR 0029).

## Where to read more

- [`docs/architecture/`](docs/architecture/): state authority, trust boundaries, data
  flows, failure and threat models.
- [`docs/decisions/`](docs/decisions/): the accepted ADRs.
- [`docs/reference/`](docs/reference/): the artifact catalog and the defects the
  rewrite resolved.
- [`docs/generated/current-workspace.md`](docs/generated/current-workspace.md): the
  generated package and dependency inventory.
