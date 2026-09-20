---
status: ACCEPTED
owner: security
source_of_truth: plan/migration/05-security-and-authority.md
last_verified_commit: 800df24076c7
verification: security contract and adversarial suites introduced in Phases 2-7
---

# Trust boundaries

## Principals and boundaries

| Boundary | Untrusted side | Trusted verifier/enforcer | Evidence required |
|---|---|---|---|
| Public API | User/client input and bearer material | Gateway identity verifier and policy decision | Authenticated subject, audience, freshness, action/resource decision. |
| Guest API | Workload process, VM ID, network address, database/role fields | Guest gateway and provider role boundary | Canonical signature/challenge, nonce, workload key epoch, trusted creature binding. |
| Federation | Remote node and forwarded claims | Destination gateway/policy | Signed envelope/response, descriptor trust, replay/hop/dedupe checks. |
| Node to VMM | Network and backend claims | mTLS VMM endpoint and contract validator | Service identity, operation ID, desired generation, capability profile. |
| VMM to worker | Scheduler allocation and host request | Restricted worker agent | Workload/allocation binding, permitted operation, short-lived identity. |
| Application to module | Independently installed artifact/process | Module supervisor and contract boundary | Artifact signature, publisher trust, manifest permissions, negotiated version. |
| Application to storage | Provider and physical representation | Typed port, migration generation, provider credentials | Provider identity/capabilities and selected binding. |
| Creature database | Other creatures and caller-controlled names | Provider-native database/namespace and role permissions | Server-resolved binding; dedicated role; verified session reset. |
| Operator/bootstrap | Scripts, environment, recovery input | Typed config and signed/resumable journal | Explicit source precedence, secret handling, signature and revision. |

Network location, process co-location, a workload identifier, and possession of a
provider job/allocation ID never confer authority. Internal service calls use mutually
authenticated, rotated identities.

## Secret boundaries

- Long-lived provider and database administrator credentials remain only in the
  composition/supervisor secret boundary.
- Workloads receive neither database passwords nor a role/database selector.
- Modules receive only declared, scoped secrets. Secret values never enter manifests,
  logs, health payloads, support bundles, or bootstrap snapshots.
- Signing keys are separated by user, node, module publisher, service, and workload;
  rotation epochs and revocation are explicit.
