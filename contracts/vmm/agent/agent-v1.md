---
status: ACCEPTED
owner: vmm/agent
source_of_truth: this contract, ADR 0010, plan/migration/04-vmm-nomad-and-runtimes.md
last_verified_commit: eebb9c5
verification: cargo test -p aseman-vmm-agent; live Firecracker v1.17.0
---

# A603: the worker agent protocol and privilege model (v1)

`aseman-vmm-agent` does the privileged host work a microVM needs — `/dev/kvm`, tap
devices, cgroups, the jailer, process lifecycle — so that nothing else has to. The node
is unprivileged, the VMM service is unprivileged, and the VMM backend holds a scheduler
token and nothing more (A602). Privilege is concentrated in one small component that can
be audited, and disabled per host.

## Why an agent and not a task driver

A privileged Nomad task-driver plugin would tie the only Firecracker implementation to
Nomad's plugin ABI and put Aseman's privilege inside the scheduler. The agent keeps
scheduler integration replaceable: Nomad schedules an *unprivileged runner task*, and
the backend asks the local agent to do the host work for that allocation (ADR 0010).
The native backend can call the same agent.

## Authentication

The agent listens on a host-local address over mutual TLS, and admits exactly the client
certificates its configuration lists — in practice, the backend on the same host.

Mutual TLS proves which component is calling. It does not say what may be done, so every
request also carries a **grant**: a short-lived, signed statement naming one allocation,
one machine profile, and a deadline. The agent checks:

1. The signature, against the VMM's configured public key.
2. The deadline, against its own clock, with no tolerance for a stale grant.
3. That the allocation named in the grant is the allocation the request operates on.
4. That the machine profile named is one the administrator declared on this host.

A request whose grant does not cover it is refused. There is no "trusted caller" path.

## Operations

| Operation | What it does |
|---|---|
| `create` | Prepare a microVM for an allocation from a declared profile: allocate its directory under the agent's root, write the Firecracker configuration, start the VMM process with its API socket |
| `start` | Boot it |
| `pause` / `resume` | The runtime's own pause, through Firecracker's API — not a stop and a start |
| `state` | What the microVM is actually doing |
| `stop` | Shut it down |
| `delete` | Stop it and remove its directory |

Every operation is idempotent and keyed by allocation: repeating one does not create a
second machine, and a `delete` of something already gone is success.

## What the agent does not expose

- No general shell, and no command execution of the caller's choosing.
- No arbitrary filesystem path. Every path derives from the agent's own allocation
  root; a path that escapes it is refused, not normalized.
- No arbitrary device. Devices come from the administrator's profile.
- No raw Firecracker API. The agent speaks to Firecracker; callers speak to the agent.
- No network of the caller's choosing. Tap devices and address ranges are the
  administrator's declared profiles.

## Profiles

An administrator declares machine profiles per host: vCPU count, memory, the kernel and
root image to boot, and the network profile. A grant names a profile; it never carries a
machine specification of its own. This is what keeps a compromised VMM from asking for
a microVM with the host's disk attached.

## Capability, and its absence

A host without `/dev/kvm` cannot run microVMs. The agent reports that plainly at
startup and refuses `create`, rather than accepting work it cannot do. An operator can
also disable the Firecracker capability on a host that has KVM, independently of
everything else the agent does.

Failures are reported as they are: a boot that fails for want of a kernel says so, and
a host without KVM says so. The agent never reports a microVM as running because the
configuration was accepted.

## Refusals

Stable reasons:

- `the grant does not cover this allocation`
- `the grant has expired`
- `the grant is not signed by the VMM`
- `unknown machine profile`
- `this host has no KVM`
- `the firecracker capability is disabled on this host`
- `the path escapes the allocation root`
- `unknown allocation`
