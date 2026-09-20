# Module System

## Boundary model

Runtime-replaceable modules are processes or OCI artifacts, not Rust dynamic libraries. This avoids unstable Rust ABI coupling and permits modules written in other languages. Rust traits remain useful behind generated clients and servers.

Distribution uses either a digest-pinned signed OCI image or a signed `.amod` bundle containing the manifest, native executable, schemas, SBOM, license metadata, and signatures. Remote managed services use a signed endpoint descriptor with the same contract and identity requirements.

Terminology is strict:

- **Port**: an in-process behavioral interface required by application code.
- **Adapter**: in-process translation between a port and a wire/domain boundary.
- **Module**: an independently installed and supervised process/OCI artifact.
- **Provider**: a module implementing one replaceable platform capability.
- **Runtime driver**: VMM-side code/module controlling one workload technology.
- **Service**: a long-running first-party executable such as `aseman-node` or `aseman-vmm`.

Module kinds include:

- Storage provider.
- Client network adapter.
- Federation adapter.
- Security/policy provider.
- Realtime provider.
- Finance ledger provider.
- Consensus provider.
- Coordination/lease provider.
- VMM service or VMM-backend provider.
- Runtime driver/runner.
- Telemetry exporter.

## Manifest

Every module ships a signed `module.toml` containing:

```toml
name = "storage-postgres"
kind = "storage"
version = "1.0.0"
contract = ">=1.0,<2.0"
artifact_digest = "sha256:..."
command = ["/usr/bin/aseman-storage-postgres"]
health_endpoint = "/health/ready"
config_schema = "config.schema.json"
capabilities = ["transactions.multi_capsule", "indexes.unique", "guest_database.isolated_roles", "guest_database.ddl"]
```

It also declares required permissions, network access, filesystem mounts, secrets, migrations, upgrade compatibility, and supported platform/architecture.

## Control and data protocols

- Module supervision uses a versioned control protocol over a Unix socket on one host or mTLS TCP across hosts.
- The VMM's node-facing contract is HTTP/JSON with SSE/WebSocket streams as specified in [04-vmm-nomad-and-runtimes.md](04-vmm-nomad-and-runtimes.md).
- Storage, security, realtime, finance, consensus, and VMM-backend modules use versioned protobuf/gRPC data contracts over Unix sockets or mTLS TCP unless their contract explicitly requires HTTP.
- Network modules own public protocol parsing and translate to/from a canonical gateway RPC; they cannot invoke application internals directly.
- Wire contracts, compatibility fixtures, and generated clients live under `contracts/` and `aseman-contracts`.

The module supervisor and generated clients hide transport details from application ports. A third-party implementation may use any language if it passes the wire-level conformance suite.

Storage providers serving guest data additionally expose database/namespace provisioning, dedicated role/principal lifecycle, constrained schema management, and safe role-assumption operations. The Aseman guest-data proxy authenticates signed workload requests and supplies the trusted creature binding separately; storage modules never accept a workload-selected database, role, or namespace as authority.

## Lifecycle

1. Install and verify signature/digest.
2. Validate configuration and contract range.
3. Run provider conformance tests.
4. Generate and preview migrations or operational changes.
5. Start candidate in isolation.
6. Wait for readiness and warm-up.
7. Atomically route new work to the candidate.
8. Drain the old module.
9. Observe a rollback window.
10. Stop the old module or roll back routing.

## Minimal-restart semantics

"Plug and play" does not mean every stateful provider can change instantaneously:

- Stateless network adapters can normally switch through a proxy and connection drain.
- Realtime providers require offset/checkpoint transfer.
- Security providers switch at a policy/key epoch.
- Consensus providers switch at a finalized financial epoch.
- VMM endpoint/backend changes require workload cordon, migration/recreation, and reconciliation.
- Storage providers require schema creation, backfill, dual writes, verification, cutover, and rollback retention.

The module supervisor minimizes node interruption but refuses unsafe activation.

## CLI

```text
asemanctl module list
asemanctl module inspect <name>
asemanctl module install <artifact>
asemanctl module validate <name>
asemanctl module activate <name>
asemanctl module drain <name>
asemanctl module rollback <name>
asemanctl module status <name>
```

Installation and activation are separate. A typical flow is:

```text
asemanctl module trust add publisher.pem
asemanctl module install <signed-oci-or-amod> --scope cluster
asemanctl module configure <name> --file module.toml
asemanctl module validate <name>
asemanctl module stage <name>
asemanctl module status <name>
asemanctl module activate <name>
```

The CLI calls the authenticated node administration API. It does not copy arbitrary code into the node process. Install verifies and caches an artifact; stage starts it in isolation and executes its conformance tests; activate changes a versioned routing generation only after module-specific safety work succeeds.

Storage activation delegates to the capsule migration workflow. Network activation stages the adapter behind a listener broker, sends new connections to it, and drains old connections. Security and consensus changes use explicit epochs. VMM changes cordon and reconcile workloads.

For cluster scope, the master records desired module state, distributes only signed digest-pinned artifacts/configuration, waits for the required placement quorum, and then advances the routing generation. Every host verifies the artifact independently.

The authoritative module registry is stored as capsules. A minimal signed local bootstrap snapshot contains only last-known-good provider endpoints, versions, digests, trust roots, and secret references so the node can reconnect after restart. This snapshot is a derived recovery cache, not an independent source of business state.

## Supply-chain requirements

- Signed OCI artifacts or packages.
- Digest pinning.
- SBOM and license manifest.
- Vulnerability scan results.
- Least-privilege declared permissions.
- Reproducible release metadata.
- Contract and compatibility test reports.
