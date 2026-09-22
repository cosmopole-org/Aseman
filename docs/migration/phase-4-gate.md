---
status: ACCEPTED
owner: migration/phase-4
source_of_truth: plan/migration/09-migration-phases.md (Phase 4)
last_verified_commit: 5c6e6eb
verification: cargo xtask full; live PostgreSQL identity, gateway, and guest data tests
---

# Phase 4 exit gate

## Decision

**Accepted.** Identities, capability grants, policy decisions, and the guest gateway
are implemented against accepted contracts (A401-A406), and every registered action is
enforced at one decision point. The gate's property and adversarial requirements are
covered clause by clause in the P4-05 record.

## Work items

| Item | Status | Record |
|---|---|---|
| User/node/service/workload identities and rotation | Delivered | P4-01, A401 |
| Capability grants, policy decisions, expiry, revocation, explanations | Delivered | P4-02, P4-03, A402-A404 |
| Enforce authorization for every operation | Delivered | P4-05 |
| Attenuated child-workload delegation | Delivered | P4-03 (intersection-only delegation, property test) |
| Move host calls to the authenticated guest API | Delivered | P4-04 (unified host call with node identity; signed gateway for out-of-process workloads; guest data on creature databases) |
| Signed workload authentication and trusted workload-program-creature-database resolution | Delivered | P4-04, A405 |
| Deny-by-default workload network and secret policy | Delivered for host-mediated access | A406; egress in shadow until grants exist |

## Security defects closed

- **LD-14:** unauthorized VM creature host calls.
- **LD-24:** guest raw node keys (ADR 0028).
- **LD-25:** deploy file traversal.
- **LD-26:** resource entity traversal.
- **LD-27:** forged caller identity.

## Owned by later phases

- **Direct network access by container and microVM workloads** is P6. The runtime
  providers enforce the same `network.egress` grants.
- **Transports for the signed guest gateway and identity lifecycle** are P7-01, P7-02,
  and P7-03: the public API, descriptors, and federation.
- **Ending the shadow exceptions:** egress grants (A406), RL-019 email login, and P7-01
  discovery scopes.
