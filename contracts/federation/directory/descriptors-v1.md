---
status: ACCEPTED
owner: federation
source_of_truth: this contract, aseman-domain::federation
last_verified_commit: eebb9c5
verification: cargo test -p aseman-domain federation
---

# A704: node and workload descriptors (v1)

## The node descriptor

A node publishes, signed with its node key: its stable node ID (bound to that key), its
key epoch and active keys, its federation and client endpoints, the contract versions it
speaks, the runtime classes it offers, a sequence number, an expiry, and the key epochs
it has revoked.

**Sequences never move backwards.** A cached descriptor is replaced only by one with a
strictly higher sequence, so a replayed older descriptor cannot un-rotate a key or
un-revoke an epoch. A descriptor that names its own `key_epoch` among `revoked_epochs`
is refused outright: a node cannot publish keys it has itself revoked.

One Aseman node is one node ID, whatever its internal topology. Adding or removing a
worker, or replacing a control replica, never changes it (ADR 0013, A602).

## The workload descriptor

Every registered workload has a signed minimal descriptor: its workload ID, its home
node ID and address, its public key, a revision, and an expiry.

It is deliberately small. It says where to send something and how to verify the answer,
and nothing about the creature, the program, its capabilities, its logs, its presence,
or its data. Any authenticated workload in the federation may resolve one by stable
workload ID; bulk enumeration and extended metadata stay policy-controlled.

A cached workload descriptor is replaced only by a higher revision.

## Caches

The home node is authoritative. A cache holds a descriptor until its expiry, replaces it
only on a higher sequence or revision, and drops it on a revocation update. A descriptor
past its expiry is not used.

## And still

Resolving a descriptor grants nothing. It is how a request finds its destination, not
permission to make it.
