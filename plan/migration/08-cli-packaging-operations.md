# CLI, Packaging, and Operations

## Administrative CLI

Required command groups:

```text
asemanctl module install|validate|activate|drain|rollback|status|list
asemanctl node init|start|status|doctor|upgrade|backup|restore
asemanctl cluster bootstrap|add-server|add-worker|cordon|drain|remove
asemanctl vmm status|workloads|logs|exec|reconcile
asemanctl policy grant|revoke|explain|list
asemanctl federation trust|revoke|peers|discover|doctor
asemanctl storage providers|capabilities|schema|migration
asemanctl storage guest-database status|doctor|reconcile
asemanctl meter usage|status|reconcile
asemanctl finance balance|journal|settle|reconcile
```

Every mutating command supports structured output, request/idempotency ID, dry-run where meaningful, explicit target, progress reporting, timeout, and audit correlation.

## Containers

Publish separate images for:

- `aseman-node`.
- `aseman-vmm`.
- `aseman-meter`.
- Optional provider services.

PostgreSQL, the durable realtime provider, and Nomad use their own images/packages. Containers run as non-root with read-only roots, minimal capabilities, pinned versions, health checks, secrets mounts, and resource limits. The VMM controller does not receive KVM or broad host privileges; only the worker agent on eligible hosts receives narrowly required access.

## Deployment profiles

- Compact Compose profile: one machine, single Nomad server/client, all required services.
- Cluster profile: multiple Aseman API/control replicas behind one stable node endpoint, three/five Nomad servers, multiple workers, external/HA PostgreSQL, coordination, and realtime services.
- Host/systemd profile: worker agents and Nomad clients where containers cannot safely expose KVM or networking functions.

## Bootstrap

Replace the giant imperative installer with an idempotent `asemanctl bootstrap` workflow and small platform-specific helpers.

It must:

1. Check OS, architecture, cgroups, KVM, ports, DNS, time synchronization, disk, memory, and certificates.
2. Select compact or clustered topology.
3. Pin and verify artifact versions/checksums.
4. Generate keys, certificates, secrets, and typed configuration.
5. Apply database schemas and provider configuration.
6. Start dependencies in order and wait for readiness.
7. Resume safely after interruption.
8. Roll back the current failed stage without destroying working data.
9. Run end-to-end health checks.
10. Emit a redacted support bundle.

## Observability and recovery

- Unified structured logging and OpenTelemetry-compatible traces.
- Prometheus-compatible metrics.
- Liveness/readiness endpoints for every service.
- Backup manifests contain capsule schema versions, provider mappings, module versions, and integrity hashes.
- Restore is tested onto a clean deployment.
- Runbooks cover worker loss, provider failure, database cutover rollback, key compromise, federation partition, and billing reconciliation.

## CI and release

- `cargo fmt --check` and Clippy with warnings denied.
- Unit, contract, integration, end-to-end, property, fuzz, load, and chaos suites.
- Test containers for PostgreSQL, realtime provider, and Nomad.
- API/module compatibility and breaking-change checks.
- Dependency, license, and vulnerability policies.
- Multi-architecture image builds.
- SBOMs, signed artifacts, and provenance.
- Release artifacts published outside the source tree; generated binaries are not committed.
