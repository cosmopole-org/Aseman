---
status: ACCEPTED
owner: security/authority
source_of_truth: this contract, ADR 0008, contracts/security/actions.json
last_verified_commit: 5c6e6eb
verification: python3 scripts/generate_security_registry.py --check; cargo test -p aseman-domain authority; cargo test -p aseman-policy-native
---

# A402 and A404: action registry and policy decisions (v1)

## A402: the registry

`actions.json` is the maintained registry. It lists:
- every subject class (A401 section 1);
- every resource type;
- every condition;
- every action.

For each action it records:
- its resource type and class (`read`, `write`, `security`, `financial`,
  `administrative`);
- the subject classes that may ever hold it;
- its rule;
- whether grants for it may be delegated;
- the legacy guard it replaces, and its removal when it is scheduled to disappear;
- the current surfaces it covers.

`scripts/generate_security_registry.py --check`, which runs in the gate, enforces:
- **Coverage:** every inventoried surface (A002 signed actions, guest operations, HTTP
  routes, and the P2 module admin API) maps to exactly one action, and no action claims
  a surface that no longer exists. A new surface without an action fails the gate.
- **Consistency:** IDs, resources, subjects, conditions, and guards all exist.
- **Forbidden actions:** `never` stands alone and names its removal.
- **Public actions:** a `security`, `financial`, or `administrative` action cannot be
  public. The only exceptions are the legacy session handshake and identity challenges.

A rule lists conditions; the action is allowed when the first condition that holds is
found. Rules are the least-privilege replacement of the legacy guards (ADR 0008:
ambiguous legacy access grants nothing). `raw_state.*`, the LD-24 raw key access, and
custodial email login are `never`.

## A404: decisions

**Request.** A request carries:
- the subject (absent for an anonymous caller);
- the action ID;
- the resource (type and ID);
- the facts;
- the decision time.

Facts are the relations the caller established before asking: `self`, `owner`,
`same_creature`, `store_*`, `secret_grantee`, `counterparty`, `finance_operator`, and
`node_admin`. Evaluation performs no I/O. `granted` is never a fact: it holds only for a
matching A403 capability grant.

**Evaluation order.** The first failing step decides:

1. The action is registered, else `unknown_action`.
2. The resource type is the action's, else `resource_mismatch`.
3. The rule is not `never`, else `forbidden`.
4. An anonymous caller is allowed only a `public` rule (`matched: public`), else
   `authentication_required`.
5. The subject class may hold the action, else `subject_not_allowed`.
6. The first rule condition that holds allows (`allowed`, `matched` names it), else
   `condition_not_met`.

**Decision.** Every decision reports:
- `allowed`
- the `reason` code
- `matched`, the explaining condition
- the conditions `considered`
- the registry and policy versions

A provider that cannot decide reports `provider_error`, and callers deny.

**Conformance.** `tests/contracts/policy/decisions-v1.json` holds the normative
hand-written cases. Every provider must reproduce them with
`aseman_policy_conformance::check_provider` before activation. The reference provider
is `aseman-policy-native`.

## A403: capability grants

**The grant.** A grant (`core.capability_grant`) holds:
- its subject and issuer
- an action set
- a resource selector: `exact` (one resource) or `any_of_kind`, which only an
  administrator should issue as a root
- the delegable subset of its actions
- a remaining delegation depth
- its parent
- `not_before`, an optional `expires_at`, and `revoked_at`
- the policy version

**Chains.** A chain is a grant followed by its ancestors to its root. The chain
authorizes only while every link holds:
- **Live:** inside its window and not revoked.
- **Linked:** each child's parent is the next grant, and its issuer is that grant's
  subject.
- **Attenuating:**
  - the child's actions are among the parent's delegable actions;
  - its delegable actions are among its own actions;
  - its resource is within the parent's;
  - it starts no earlier and expires no later than the parent;
  - its depth is below the parent's, and the parent's depth is at least 1.

Evaluation re-checks all of this, so a tampered stored chain authorizes nothing.
Revoking or expiring any ancestor voids every descendant at once. The revocation use
case also returns the voided subtree for re-evaluation, for example to suspend
workloads.

**Delegation.** Delegation is intersection only. The child receives:
- the requested actions, limited to the parent's delegable actions and to the actions
  the registry marks delegable for the child's class and the resource type;
- the intersection of the two selectors;
- the later start and the earlier expiry;
- `min(requested depth, parent depth - 1)`.

Delegation cannot extend duration, scope, actions, or depth. A randomized property test
(`aseman-domain` capability tests, 2,000 rounds of up to four delegations) checks this
for every grant it produces.

**Authorization.** The caller loads the subject's grants holding the action, with their
chains, bounded to 16 links, and passes them in the request. When `granted` allows, the
decision's `grant_chain` names the authorizing chain, grant first and root last. Issuing,
delegating, and revoking are themselves registry actions (`capability.issue`,
`capability.delegate`, `capability.revoke`).
