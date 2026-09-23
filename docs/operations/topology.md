---
status: CURRENT
owner: operations
source_of_truth: contracts/deploy/topology.json (A602), ADR 0002, ADR 0010, ADR 0013
last_verified_commit: eebb9c5
verification: cargo xtask fast (the topology contract is checked against the profiles)
---

# Deployment topology

Written for: the operator who installs and grows an Aseman node.

One Aseman node is one federation identity, in every profile. Adding a worker, losing a
control replica, or switching the VMM backend never changes the node ID or the address
federation knows it by. Everything below follows from that.

## Profiles

### Compact

One machine: one Aseman control replica, one Nomad server that is also a client, one
PostgreSQL, one VMM service, one backend.

The single replica still takes the coordination lease before it does singleton work. It
looks like overhead on one machine, and it is the reason a compact deployment can grow
into a cluster without changing how anything is fenced.

### Cluster

Three or more Aseman control replicas behind one stable HTTPS endpoint, three or five
Nomad servers, a worker pool you add to and remove from, and external or HA PostgreSQL.

The replica count needs no quorum: PostgreSQL owns application state and coordination,
not a replica vote (ADR 0013). Nomad's own Raft quorum is separate and is three or five
servers.

### Host

Worker agents and Nomad clients installed on the host rather than in a container, for
machines where a container cannot safely be given `/dev/kvm`, tap devices, or cgroup
control. Firecracker needs those, and they stay on the host and out of every other
service (ADR 0010).

## What listens where

| Service | Port | Reachable by | Presents |
|---|---|---|---|
| `aseman-node` API and guest API | 443 | users and federated nodes; workloads reach `/guest/v1` from the worker network | the stable endpoint's certificate |
| `aseman-node` health | 8080 | the load balancer and the operator | — |
| `aseman-vmm` A501 | 8443 | the Aseman node only, over mutual TLS | a certificate from the VMM CA |
| VMM backend A504 | 9090 | the VMM service, on loopback only | nothing: the listener refuses a non-loopback address |
| `aseman-vmm-agent` | 9091 | the backend on the same host | a host certificate from the VMM CA |
| Nomad servers | 4646, 4647, 4648 | the backend, operators, and other Nomad members | Nomad's own certificates |
| Nomad clients | 4646 (host-local) | the backend, for logs, stats, and files | Nomad client certificates |
| PostgreSQL | 5432 | the node and the VMM service only | — |

Workers and workloads never reach PostgreSQL. Guest data is reached only through the
node's signed guest API, which resolves the creature server-side (ADR 0001, A405).

## Privileges

- The Aseman node has none: no KVM, no Docker socket, no host mounts.
- The VMM service has none: it schedules, it does not run workloads.
- The Nomad backend needs one namespace-scoped Nomad token.
- The native backend needs the Docker socket.
- The worker agent has `/dev/kvm`, tap devices, cgroups, the jailer, and its own
  allocation root — and nothing else (ADR 0010).

## Tokens and certificates

The Nomad token the backend uses is scoped to the `aseman` namespace and to submitting
and reading jobs, allocation lifecycle, logs, and the allocation filesystem. It has no
`operator`, `acl`, `agent-write`, or `node-write` capability: a stolen backend token
must not be able to reconfigure the cluster or read another namespace's jobs.

The VMM CA and the Nomad CA are separate, so a compromise of one does not admit the
other. The VMM admits exactly the node certificates its configuration lists, and a
node's client certificate SHA-256 is the `owner` every VMM record is scoped by.

Certificates rotate without restarting the node, and a rotation never changes the node
ID or its signing-key lineage (ADR 0009).

Aseman does not bundle, mirror, or download Nomad (ADR 0002). You supply a compatible
installation and accept its terms; the release notes record the versions tested.

## Bring-up order

PostgreSQL, then the VMM backend, then the VMM service, then the node. Each waits for
the one before it to report ready.

Migrating an existing node has its own order, and it is not optional: the Phase 3
storage cutover, then the VMM rollout, then the VMM handoff, then the new node. A remote
VMM is refused under the legacy storage provider, because the VMM's stores and the
node's workload records must be the same database (ADR 0030). The storage runbook and
the VMM handoff runbook carry the steps.

## Growing and shrinking

Adding a worker is a Nomad operation: enroll the client, and the scheduler places new
allocations on it. Removing one is a drain. Neither touches Aseman, and neither changes
the node ID — the invariant this document exists to protect. P6-03 covers enroll,
cordon, drain, and loss recovery in detail.
