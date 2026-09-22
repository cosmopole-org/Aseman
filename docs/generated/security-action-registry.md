---
status: GENERATED
owner: security/authority
source_of_truth: contracts/security/actions.json (registry 2026-09-22.4)
verification: python3 scripts/generate_security_registry.py --check
---

# A402 action registry

124 actions over 34 resource types. Every inventoried surface maps to exactly one action; unknown actions deny (ADR 0008).

## Conditions

| Condition | Meaning |
|---|---|
| `public` | No authentication: anyone, including anonymous callers |
| `authenticated` | Any authenticated subject |
| `self` | The resource is the subject itself |
| `owner` | The subject owns the resource (a creature's owner, a program's machine owner, a store's creator) |
| `same_creature` | A workload acting on resources of the creature it is bound to (A405 trusted binding) |
| `store_read` | Store membership with the `read` permission |
| `store_signal` | Store membership with the `signal` permission |
| `store_manage` | Store membership with the `manage` permission |
| `secret_grantee` | A live secret grant to the subject |
| `counterparty` | The subject is a party to the financial record (payer, payee, pool member) |
| `finance_operator` | The subject holds the finance operator role |
| `node_admin` | The subject holds the node administration role |
| `granted` | An explicit capability grant for this action and resource (A403) |
| `never` | Never allowed; the surface is removed at the phase in `removal` |

## Actions

| Action | Resource | Class | Subjects | Rule | Legacy guard | Surfaces |
|---|---|---|---|---|---|---|
| `node.diagnostics.read` | node | read | user, creature, node, service, workload, module_publisher | `public` | public | 3 |
| `node.health.read` | node | read | user, creature, node, service, workload, module_publisher | `public` | http_public | 2 |
| `node.identity.read` | node | read | user, creature, node, service, workload, module_publisher | `public` | public | 2 |
| `node.peers.read` | node | read | user, creature, node, service, workload, module_publisher | `public` | public | 1 |
| `node.id.generate` | node | write | workload | `same_creature` (removed: P4 UUIDv7 identities (ADR 0020)) | guest | 1 |
| `node.telemetry.read` | node | administrative | user, service | `node_admin` | http_public | 1 |
| `node.profiling.read` | node | administrative | user, service | `node_admin` | http_public | 7 |
| `node.cluster.administer` | node | administrative | user, service | `node_admin` (removed: RL-012 (ADR 0012)) | http_cluster | 12 |
| `node.cluster.replicate` | node | administrative | node | `node_admin` (removed: RL-012 (ADR 0012)) | http_cluster | 3 |
| `node.protocol.call` | node | write | workload | `never` (removed: P4-04 (LD-14: forwarded guest-chosen operations to the identity-less callback protocol)) | guest | 2 |
| `raw_state.read` | raw_state | read | workload | `never` (removed: P4-04 guest gateway (LD-24)) | guest | 0 |
| `raw_state.write` | raw_state | write | workload | `never` (removed: P4-04 guest gateway (LD-24)) | guest | 0 |
| `identity.session.create` | session | security | user, creature, node, service, workload, module_publisher | `public` (removed: P7-05 (legacy framing expiry)) | user | 1 |
| `identity.session.login_by_email` | session | security | user | `never` (removed: RL-019 (ADR 0019)) | public | 1 |
| `identity.signature.check` | identity_key | read | user, creature, node, service, workload, module_publisher | `authenticated` | user | 2 |
| `identity.key.rotate` | identity_key | security | user, creature, node, service, workload | `self` | new | 0 |
| `identity.key.revoke` | identity_key | security | user, creature, node, service, workload | `self` or `node_admin` | new | 0 |
| `identity.key.enroll` | identity_key | security | user, creature | `self` | new | 0 |
| `identity.challenge.issue` | challenge | security | user, creature, node, service, workload, module_publisher | `public` | new | 0 |
| `identity.trust_root.enroll` | trust_root | administrative | user, service | `node_admin` | new | 0 |
| `identity.introduction.accept` | trust_root | administrative | node | `granted` | new | 0 |
| `identity.login_grant.issue` | login_grant | security | workload | `same_creature` | guest | 1 |
| `identity.bridge_token.issue` | bridge_token | security | workload | `same_creature` | guest | 1 |
| `identity.bridge_token.revoke` | bridge_token | security | workload | `same_creature` | guest | 1 |
| `creature.create` | creature | write | user, creature, workload | `public` or `owner` or `same_creature` | public | 3 |
| `creature.read` | creature | read | user, creature, workload | `authenticated` | user | 3 |
| `creature.discover` | creature | read | user, creature, workload | `authenticated` | user | 1 |
| `creature.list` | creature | read | user, creature, workload | `owner` or `granted` | user | 3 |
| `creature.update` | creature | write | user, creature, workload | `self` or `owner` or `same_creature` | user | 3 |
| `creature.delete` | creature | write | user, creature, workload | `self` or `owner` or `same_creature` | user | 5 |
| `creature.types.read` | creature | read | user, creature, node, service, workload, module_publisher | `authenticated` | user | 1 |
| `creature.signal` | creature_signal | write | user, creature, workload | `authenticated` | user | 2 |
| `finance.account.read` | account | financial | user, creature, workload | `self` or `owner` | finance | 1 |
| `finance.transfer` | account | financial | user, creature, workload | `self` or `same_creature` | finance | 2 |
| `finance.mint` | account | financial | user, service | `node_admin` | user | 1 |
| `finance.adjustment` | account | financial | user, service | `finance_operator` | finance | 1 |
| `finance.reconcile` | finance_system | financial | user, service | `finance_operator` | finance | 1 |
| `finance.lock.create` | lock | financial | user, creature, workload | `self` or `same_creature` | user | 2 |
| `finance.lock.consume` | lock | financial | user, creature, workload | `counterparty` | user | 2 |
| `finance.hold.create` | hold | financial | user, creature, workload | `counterparty` | finance | 1 |
| `finance.hold.read` | hold | financial | user, creature, workload | `counterparty` | finance | 1 |
| `finance.hold.start` | hold | financial | user, creature, workload | `counterparty` | finance | 2 |
| `finance.hold.release` | hold | financial | user, creature, workload | `counterparty` | finance | 2 |
| `finance.hold.settle` | hold | financial | user, creature, workload | `counterparty` | finance | 2 |
| `finance.pool.open` | pool | financial | user, creature, workload | `counterparty` | finance | 1 |
| `finance.pool.close` | pool | financial | user, creature, workload | `counterparty` | finance | 1 |
| `finance.pool.debit` | pool | financial | user, creature, workload | `counterparty` | finance | 2 |
| `finance.pool.refresh` | pool | financial | user, creature, workload | `counterparty` | finance | 1 |
| `finance.pool.reserve` | pool | financial | user, creature, workload | `counterparty` | finance | 2 |
| `finance.pool.release` | pool | financial | user, creature, workload | `counterparty` | finance | 2 |
| `finance.pool.settle` | pool | financial | user, creature, workload | `counterparty` | finance | 2 |
| `finance.payout.list` | payout | financial | user, creature, workload | `self` or `finance_operator` | finance | 1 |
| `finance.payout.request` | payout | financial | user, creature, workload | `self` | finance | 1 |
| `finance.payout.resolve` | payout | financial | user, service | `finance_operator` | finance | 1 |
| `finance.catalog.publish` | finance_catalog | financial | user, creature, workload | `self` or `same_creature` | finance | 2 |
| `finance.quote.publish` | finance_quote | financial | user, creature, workload | `self` or `same_creature` | finance | 2 |
| `finance.node.register` | finance_node | financial | user, creature, workload | `owner` or `same_creature` or `finance_operator` | finance | 2 |
| `finance.node.retire` | finance_node | financial | user, creature, workload | `owner` or `same_creature` or `finance_operator` | finance | 2 |
| `finance.resource.register` | finance_resource | financial | user, creature, workload | `owner` or `same_creature` | finance | 2 |
| `finance.resource.retire` | finance_resource | financial | user, creature, workload | `owner` or `same_creature` | finance | 2 |
| `finance.resource.review` | finance_resource | financial | user, creature, workload | `owner` or `same_creature` or `finance_operator` | finance | 2 |
| `secret.write` | secret | security | user, creature, workload | `self` or `same_creature` | user | 1 |
| `secret.read` | secret | security | user, creature, workload | `self` or `same_creature` or `secret_grantee` | user | 2 |
| `secret.list` | secret | read | user, creature, workload | `self` or `same_creature` | user | 1 |
| `secret.list_granted` | secret | read | user, creature, workload | `self` or `same_creature` | user | 2 |
| `secret.grant` | secret | security | user, creature, workload | `self` or `same_creature` | user | 1 |
| `secret.revoke` | secret | security | user, creature, workload | `self` or `same_creature` | user | 1 |
| `store.create` | store | write | user, creature, workload | `authenticated` or `same_creature` | guest | 2 |
| `store.read` | store | read | user, creature, workload | `store_read` or `same_creature` | guest | 2 |
| `store.update` | store | write | user, creature, workload | `store_manage` or `owner` | guest | 1 |
| `store.delete` | store | write | user, creature, workload | `owner` or `store_manage` | guest | 4 |
| `store.signal` | store | write | user, creature, workload | `store_signal` | store | 2 |
| `store.history.read` | store | read | user, creature, workload | `store_read` | store | 1 |
| `store.access.read` | store | read | user, creature, workload | `store_read` | store | 2 |
| `store.access.write` | store | security | user, creature, workload | `store_manage` or `owner` | store | 7 |
| `store.members.read` | store | read | user, creature, workload | `store_read` | guest | 3 |
| `topic.publish` | topic | write | user, creature, node, service, workload, module_publisher | `public` or `granted` | public | 3 |
| `topic.subscribe` | topic | read | user, creature, node, service, workload, module_publisher | `public` or `granted` | public | 2 |
| `topic.unsubscribe` | topic | read | user, creature, node, service, workload, module_publisher | `public` | public | 1 |
| `program.create` | program | write | user, creature, workload | `owner` or `same_creature` | user | 2 |
| `program.read` | program | read | user, creature, workload | `owner` or `same_creature` | guest | 1 |
| `program.list` | program | read | user, creature, workload | `owner` or `same_creature` | user | 4 |
| `program.update` | program | write | user, creature, workload | `owner` or `same_creature` | user | 2 |
| `program.delete` | program | write | user, creature, workload | `owner` or `same_creature` | user | 3 |
| `program.alarm.set` | program | write | workload | `same_creature` | guest | 1 |
| `entity.deploy` | entity | write | user, creature, workload | `owner` or `same_creature` | user | 3 |
| `entity.delete` | entity | write | user, creature, workload | `owner` or `same_creature` | user | 1 |
| `entity.download` | entity | read | user, creature, workload | `authenticated` | user | 1 |
| `workload.start` | workload | write | user, creature, workload | `owner` or `same_creature` | user | 2 |
| `workload.stop` | workload | write | user, creature, workload | `owner` or `same_creature` | user | 2 |
| `workload.pause` | workload | write | user, creature, workload | `owner` or `same_creature` | new | 0 |
| `workload.delete` | workload | write | user, creature, workload | `owner` or `same_creature` | guest | 2 |
| `workload.list` | workload | read | user, creature, workload | `owner` or `same_creature` | user | 1 |
| `workload.status.read` | workload | read | user, creature, workload | `owner` or `same_creature` | guest | 1 |
| `workload.endpoints.read` | workload | read | user, creature, workload | `owner` or `same_creature` | guest | 1 |
| `workload.logs.read` | workload | read | user, creature, workload | `owner` | user | 1 |
| `workload.logs.write` | workload | write | workload | `self` | guest | 2 |
| `workload.terminal.open` | workload | security | user, creature | `owner` | user | 1 |
| `workload.terminal.close` | workload | write | user, creature | `owner` | user | 1 |
| `workload.exec` | workload | security | user, creature, workload | `owner` or `same_creature` | guest | 2 |
| `workload.files.copy` | workload | write | user, creature, workload | `owner` or `same_creature` | guest | 3 |
| `workload.build` | workload | write | user, creature, workload | `owner` or `same_creature` | guest | 2 |
| `workload.builds.read` | workload | read | user, creature, workload | `owner` | user | 1 |
| `workload.resource_lock` | workload | write | workload | `same_creature` | guest | 2 |
| `workload.proof.verify` | workload | read | user, creature, workload | `authenticated` | guest | 2 |
| `workload.http_ingress` | workload | write | user, creature, node, service, workload, module_publisher | `public` or `granted` | http_public | 2 |
| `workload.shell_action` | workload | write | workload | `same_creature` | guest | 1 |
| `resource_store.read` | resource_store | read | user, creature, workload | `owner` or `same_creature` | guest | 4 |
| `resource_store.write` | resource_store | write | user, creature, workload | `owner` or `same_creature` | guest | 4 |
| `resource_store.delete` | resource_store | write | user, creature, workload | `owner` or `same_creature` | guest | 2 |
| `resource_entity.write` | resource_entity | write | user, creature, workload | `owner` or `same_creature` | guest | 1 |
| `resource_entity.delete` | resource_entity | write | user, creature, workload | `owner` or `same_creature` | guest | 1 |
| `guest_data.access` | guest_data | write | workload | `same_creature` | guest | 7 |
| `network.egress` | network | security | workload | `granted` | guest | 2 |
| `file.upload` | file | write | user, creature, workload | `authenticated` | user | 3 |
| `file.read` | file | read | user, creature, node, service, workload, module_publisher | `public` | http_public | 2 |
| `module.list` | module | read | user, service | `node_admin` | module_admin | 1 |
| `module.inspect` | module | read | user, service | `node_admin` | module_admin | 1 |
| `module.install` | module | administrative | user, service | `node_admin` | module_admin | 1 |
| `module.lifecycle.change` | module | administrative | user, service | `node_admin` | module_admin | 1 |
| `module_publisher.enroll` | module_publisher | administrative | user, service | `node_admin` | module_admin | 1 |
| `capability.issue` | grant | security | user, creature, service | `node_admin` or `owner` | new | 0 |
| `capability.delegate` | grant | security | user, creature, workload | `self` | new | 0 |
| `capability.revoke` | grant | security | user, creature, service, workload | `self` or `owner` or `node_admin` | new | 0 |
