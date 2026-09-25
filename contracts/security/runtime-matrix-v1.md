---
status: ACCEPTED
owner: security/runtime
source_of_truth: this contract, plan/migration/05-security-and-authority.md (Network isolation), ADR 0008
last_verified_commit: 5c6e6eb
verification: cargo test -p aseman-node --lib authority; guest_state tests; vms runtime tests
---

# A406: network, secret, and host-access enforcement per runtime (v1)

## Enforcement point

Every host call from every runtime goes through the unified host-call dispatcher.
There, the caller is identified by the node-stamped packet (LD-27) and authorized as its
registered action by the policy provider (`apps/aseman-node/src/shell/authority.rs`, P4-05):
- The wasm and JavaScript runtimes send lifecycle, HTTP, and proof calls there. Before
  LD-14 was fixed, they had typed-packet shortcuts that bypassed identity.
- Elpian wraps every guest call in the node-stamped envelope. It used to dispatch the
  guest's raw packet.
- The docker gateway stamps the verified container identity at packet level and refuses
  unidentified containers.
- The identity-less `vm_callback` protocol serves only runtime and node events.

## Matrix

| Runtime | Guest identity | Outbound network | Inbound network | Secrets | Raw host access |
|---|---|---|---|---|---|
| wasm | node-assigned VM context | host `httpRequest` only (no sockets in the sandbox), authorized as `network.egress` | `workload.http_ingress` through the VM ingress | `secret.read` for the node-resolved creature: its own secrets or unexpired grants | none; guest state confined (ADR 0028) |
| JavaScript | node-assigned VM context | as wasm | as wasm | as wasm | none; guest state confined |
| elpian | node-assigned VM context (envelope) | as wasm | as wasm | as wasm | none |
| elpify | node-assigned VM context | as wasm | as wasm | as wasm | none |
| docker | gateway-verified container (source address) | host calls as wasm, **plus** the container's own network (operator network `ASEMAN_GATEWAY_NETWORK`) | VM ingress to the container port | as wasm | container sandbox |
| firecracker (`fire`) | node-assigned VM context | host calls as wasm; the guest network is the worker's | VM ingress | as wasm | microVM |
| modal (remote) | node-assigned VM context | the remote provider's network | provider endpoint | as wasm | provider sandbox |

## Default deny

- **Host-mediated egress** (`network.egress`) is denied unless a capability grant names
  the destination host (resource `network:{host}`, A403). Grants are PostgreSQL state,
  so on the legacy provider no grant exists, and the action runs in *shadow* mode: it
  is decided and logged but not refused. Existing workloads keep working until
  operators issue their egress grants. Shadow mode ends when the node runs on
  PostgreSQL and the grants exist. Removing `network.egress` from `SHADOW_ACTIONS` is
  the switch.
- **Secrets** are denied by default: a workload reads only its own creature's secrets,
  or secrets granted to it with an unexpired grant.
- **Guest data** is confined to the creature (A405, ADR 0028). Raw node keys are
  `never`.
- **Ingress** to workload HTTP servers is public by legacy design (published creature
  apps). A grant can narrow it per workload.

## Not enforced by the host (owned by later phases)

- **Direct network access by container and microVM workloads** (docker, firecracker,
  modal) bypasses host calls. Deny-by-default there needs an internal network and an
  egress proxy per workload. That is owned by the P6 runtime providers (Nomad
  allocation networking, worker agent), which must enforce the same `network.egress`
  grants.
- **mTLS between internal services** belongs to P6/P7.
