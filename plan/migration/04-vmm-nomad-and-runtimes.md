# VMM, Nomad, and Runtimes

## Service boundary

`aseman-node` communicates with a VMM endpoint only through a versioned HTTP/OpenAPI contract protected by mTLS. The reference endpoint is the provider-neutral `aseman-vmm` service. The node never calls Nomad, Docker, Firecracker, QEMU, a VMM backend, or a runtime library directly.

`aseman-vmm` supplies common authentication, idempotency, operation tracking, reconciliation, events, and API semantics. It delegates infrastructure work through an internal versioned VMM-backend contract to independently installed modules such as `modules/vmm-backend/nomad` and `modules/vmm-backend/native-legacy`. Switching those backends does not rebuild either node or VMM service. A third-party VMM may instead implement the complete node-facing HTTP contract directly and must pass the same conformance suite.

Aseman owns desired workload state, identity, policy, and finance. The VMM owns observed runtime state. Commands are idempotent and long actions return durable operation IDs. Reconciliation closes gaps after crashes or partitions.

## HTTP contract

```text
GET    /v1/capabilities
POST   /v1/workloads
GET    /v1/workloads
GET    /v1/workloads/{id}
POST   /v1/workloads/{id}/start
POST   /v1/workloads/{id}/stop
POST   /v1/workloads/{id}/pause
POST   /v1/workloads/{id}/resume
DELETE /v1/workloads/{id}
POST   /v1/workloads/{id}/exec
GET    /v1/workloads/{id}/logs
GET    /v1/workloads/{id}/events
GET    /v1/workloads/{id}/usage
GET    /v1/operations/{operation_id}
GET    /health/live
GET    /health/ready
GET    /version
```

Requirements:

- OpenAPI 3.1 and generated client/server types.
- Idempotency keys for mutations.
- Optimistic resource versions.
- Typed problem responses.
- Deadline, cancellation, request ID, and trace propagation.
- Cursor pagination.
- SSE for logs/events and WebSocket for interactive terminal sessions.
- Explicit runtime capability negotiation.
- One conformance suite for every provider.

## Providers

In this document, a provider is a backend behind the reference `aseman-vmm` facade. It does not change the node-facing API.

### Native compatibility provider

Extract the current integrated VMM into `modules/vmm-backend/native-legacy`. Preserve valid runtime behavior while deleting its access to node internals, storage handles, shell actions, finance, and global application state. Guest host calls move to the authenticated Aseman guest API.

### Nomad default provider

Map Aseman desired workloads to Nomad jobs, task groups, tasks, and allocations. Nomad servers form the VMM control-plane cluster; Nomad clients are physical workers.

- Compact mode: one host runs a Nomad server and client plus Aseman services.
- Cluster mode: three or five Nomad servers and an expandable client pool.
- Federation sees the whole internal Nomad cluster as one Aseman node with one stable node identity.
- Worker addition/removal must not change federation identity.

Runtime mapping:

- Docker: Nomad Docker driver.
- QEMU: Nomad QEMU driver when suitable.
- WASM, JavaScript, Elpian, and Elpify: initially hardened OCI runner tasks; use custom task drivers only when justified.
- Firecracker: an audited VMM worker agent or custom task driver on KVM-capable workers.
- External hosted systems: separate provider/runtime adapters.
- Nomad `raw_exec`: forbidden as a production default because it does not isolate workloads.

## Worker agent

`aseman-vmm-agent` performs privileged host-local runtime work through a narrow authenticated API. It supports runtime-specific pause/resume, snapshots, terminal attachment, stats collection, network policy, and Firecracker control. The central node and VMM controller remain unprivileged.

Pause/resume semantics are normalized by the VMM contract but implemented per runtime. Unsupported semantics must be explicitly reported during scheduling; they may not silently degrade to stop/start.

## Workload access

Nomad workload identity proves which allocation is calling. Aseman-issued workload credentials and policy decide which Aseman operations it may perform. Nomad identity alone never authorizes node APIs.

All workloads start with:

- No Aseman API rights except minimal identity/bootstrap.
- Denied network egress and ingress unless policy grants it.
- No host filesystem or raw device access.
- No direct provider/database network access or credentials; guest data is reachable only through signed requests to the Aseman proxy, which assumes the resolved creature's restricted role.
- Short-lived secrets and capability tokens.

## Endpoint and backend switching

Switching a VMM endpoint or its backend follows: validate -> start candidate -> cordon old -> reconcile desired state -> migrate/recreate supported workloads -> verify health -> switch scheduling -> drain old -> retain rollback data. Stateful workloads require explicit volume and snapshot compatibility checks.
