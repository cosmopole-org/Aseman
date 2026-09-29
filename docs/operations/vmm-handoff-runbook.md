---
status: CURRENT
owner: vmm
source_of_truth: ADR 0022, ADR 0030, docs/migration/work-units/P5-05.md
verification: cargo test -p aseman-storage-rocksdb vmhandoff
---

# Rolling a node onto its VMM (ADR 0030) and adopting its legacy instances

A node of this version runs no runtimes: programs run as workloads of a VMM. Do this
in order; each step is safe to stop at.

## 1. Prerequisites

1. The Phase 3 cutover is complete: `ASEMAN_CORE_STORAGE_PROVIDER=postgres` with its
   guest proxy (`docs/operations/storage-migration-runbook.md`). The node refuses a
   VMM endpoint on the legacy provider.
2. A VMM database for `aseman-vmm`, of its own: the service keeps its state there on
   the PostgreSQL storage provider and migrates it at startup (ADR 0038). A database
   from an earlier release has its `aseman_vmm` tables imported at startup and moved
   to `aseman_retired`.
3. Certificates:
   - the VMM's server certificate and the CA its clients chain to;
   - this node's client certificate, whose SHA-256 fingerprint is listed in
     `ASEMAN_VMM_CLIENTS` as `node-id=fingerprint`;
   - the node's guest API server certificate, whose CA the backend trusts.

## 2. Start the backend and the service

1. `aseman-vmm-backend-native 127.0.0.1:PORT backend.json`, where `backend.json` is
   `{"state_dir": "...", "node_ca": "<the node's guest API CA>", "vm_types": [...],
   "runtime": {...}}`. `vm_types` names the runtimes to serve (every compiled one when
   absent); `runtime` holds their settings (`aseman_config::RuntimeConfig`: docker
   gateway port and network, Firecracker, Modal with its credentials as secret files,
   the storage root), which no longer come from the environment. Give it the same
   storage root the node used, so docker sandboxes keep their contents. On a compact
   deployment, `asemanctl bootstrap --backend native` writes this file.
2. `aseman-vmm` with `ASEMAN_VMM_LISTEN`, its TLS material, `ASEMAN_VMM_CLIENTS`,
   `ASEMAN_VMM_DATABASE_URL_SECRET`, and
   `ASEMAN_VMM_BACKEND_ENDPOINT=http://127.0.0.1:PORT`.
3. Check `GET /health/ready` (the plain health listener, when configured).

## 3. Adopt or stop the legacy instances (ADR 0022)

With the node **stopped**:

1. `aseman-node vmm-handoff plan handoff.json` — every legacy `VmInstance` with its
   runtime, and every external handle (containers, Modal app, image, volume, sandbox).
   It prints the plan's digest.
2. Review it and write the decisions:

   ```json
   {
     "instances": {"7@global::main::vm-1": "adopt", "7@global::web::vm-2": "stop"},
     "released": ["VmContainerName::7@global::web::vm-2"],
     "kept": ["ModalVolume::7@global"]
   }
   ```

   - Every instance needs `adopt` (run it again as a workload) or `stop`.
   - Every external handle needs `released` (you released it outside Aseman) or
     `kept` (its runtime module adopts it later — Modal volumes hold user data,
     P6-06). A stopped instance's container must be released, never kept.
3. `aseman-node vmm-handoff apply handoff.json decisions.json`. It refuses if the
   store changed since the plan, runs each adopted instance as a workload, and only
   then removes the decided observed records. Re-running it is safe.

## 4. Start the node

Set `ASEMAN_VMM_ENDPOINT` (https), `ASEMAN_VMM_SERVER_CA`,
`ASEMAN_VMM_CLIENT_IDENTITY_SECRET`, `ASEMAN_GUEST_API_URL`,
`ASEMAN_GUEST_API_CERTIFICATE`, and `ASEMAN_GUEST_API_KEY_SECRET`, then start it.
Deploy, run, stop, and delete now go to the VMM, and a workload's host calls come back
to the guest API signed with its own key.

## 5. Checks

- `/programs/runEntity` returns a `vmId`, and the VMM reports the workload running.
- A creature's signal reaches its program (the entity's signal workload).
- Logs: `GET /v1/workloads/{id}/logs` on the VMM.
- After stopping the backend, the VMM reports the workload lost and restarts it.

## Known limits

- **LD-28**: an unprivileged backend cannot purge a docker sandbox a root container
  wrote into. Run the backend as the user owning the storage root, or build creature
  images that run as that user, until the P6 worker agent does privileged host work.
- Firecracker needs `/dev/kvm`; Modal needs its account credentials. Neither is
  verified by this repository's tests.
