# Security and Workload Authority

## Zero-trust baseline

Every user, node, service, and workload has a cryptographic identity. Network location, container IP, Nomad allocation identity, or possession of a VM ID is never sufficient authority.

A single policy decision point evaluates every sensitive action:

- Create, start, stop, pause, resume, snapshot, migrate, and delete workload.
- View logs or resource usage.
- Open or observe a terminal and execute commands.
- Bulk-discover nodes/workloads or read non-minimal metadata; authenticated workloads may resolve the minimal federated descriptor required by the universal identity directory.
- Send/receive signals and access realtime topics.
- Read/write guest data.
- Access secrets, storage, network egress, and federation routes.
- Delegate rights or create child workloads.
- Perform financial or administrative actions.

## Capability model

A grant contains:

```text
subject
action set
resource selector
conditions
issuer
issued-at / not-before / expiry
delegable action subset
maximum delegation depth
policy version
revocation reference
```

Short-lived signed capability tokens are audience-bound and include nonce/replay protection. Durable grants remain in capsule storage and are auditable.

## Child workloads

When a workload requests a child:

```text
child rights = requested rights
             INTERSECT parent's currently delegable rights
             INTERSECT administrator policy
```

Delegation always attenuates. It cannot extend duration, scope, actions, network access, secret access, or delegation depth. The complete parent chain is stored and included in policy explanations.

Revoking or narrowing a parent grant triggers reevaluation of descendants and may suspend affected workloads.

## Guest API

VM host calls move behind an authenticated guest gateway. For guest data, workloads sign a canonical, audience-bound request or challenge with their registered workload key; the gateway checks expiry, nonce/replay state, key epoch, and revocation. It resolves identity, creature ownership, program, workload, policy, and the trusted creature-to-database/role binding. VMs cannot nominate their caller identity, creature scope, home node, provider, database, namespace, or role.

The gateway/proxy assumes the creature's dedicated provider role only for the resolved database and operation. Workloads receive no database passwords or provider service credentials. Provider permissions are a second enforcement boundary: even a proxy-routing defect must not let a creature role access another creature's data or catalogs. Pool reuse, cancellation, error, and retry paths must reset role/session state before reassignment.

## Network isolation

- Deny-by-default workload ingress and egress.
- Per-workload allowlists and service identities.
- Enforced boundaries using the selected runtime/network provider.
- No production `raw_exec` or unrestricted host networking.
- Internal services use mTLS and rotated identities.
- Federation requests are signed and authorized at the destination.

## Keys and audit

- Separate user, node, module, and workload keys.
- Rotation epochs and overlap windows.
- Explicit revocation distribution.
- Encrypted secrets at rest and short-lived delivery.
- Every decision records actor, subject, resource, action, result, policy version, grant chain, request ID, and trace ID as an audit capsule.

## Policy provider modularity

The application depends on typed `PolicyDecision`, `GrantStore`, `TokenIssuer`, and `IdentityVerifier` ports. The initial implementation may use a Rust-native policy engine, but an alternative engine must pass the same decision/conformance suite before activation.
