use super::*;
use aseman_domain::authority::{ActionClass, DecisionReason, RegisteredAction, evaluate};
use aseman_domain::identity::SubjectKind;
use aseman_ports::PortResult;
use std::collections::BTreeMap;
use std::sync::Mutex;

const NOW: i64 = 1_800_000_000_000;

#[derive(Default)]
struct World {
    grants: Mutex<BTreeMap<Uuid, Grant>>,
}

impl GrantStore for World {
    fn grant(&self, id: Uuid) -> PortResult<Option<Grant>> {
        Ok(self.grants.lock().unwrap().get(&id).cloned())
    }
    fn grants_of(&self, subject: &Subject) -> PortResult<Vec<Grant>> {
        Ok(self
            .grants
            .lock()
            .unwrap()
            .values()
            .filter(|grant| grant.subject == *subject)
            .cloned()
            .collect())
    }
    fn children(&self, parent: Uuid) -> PortResult<Vec<Grant>> {
        Ok(self
            .grants
            .lock()
            .unwrap()
            .values()
            .filter(|grant| grant.parent == Some(parent))
            .cloned()
            .collect())
    }
    fn put(&self, grant: &Grant) -> PortResult<()> {
        let mut grants = self.grants.lock().unwrap();
        if grants.contains_key(&grant.id) {
            return Err(PortError::Conflict);
        }
        grants.insert(grant.id, grant.clone());
        Ok(())
    }
    fn revoke(&self, id: Uuid, at: i64) -> PortResult<()> {
        let mut grants = self.grants.lock().unwrap();
        let grant = grants.get_mut(&id).ok_or(PortError::NotFound)?;
        let revoked = grant.revoked_at_millis.get_or_insert(at);
        *revoked = (*revoked).min(at);
        Ok(())
    }
}

impl ClockPort for World {
    fn unix_millis(&self) -> i64 {
        NOW
    }
}

struct Registry(ActionRegistry);

impl PolicyDecisionPort for Registry {
    fn decide(&self, request: &PolicyRequest) -> PortResult<PolicyDecision> {
        Ok(evaluate(&self.0, request, "p1"))
    }
}

fn registry() -> ActionRegistry {
    let action = RegisteredAction {
        id: "network.egress".to_owned(),
        resource: "network".to_owned(),
        class: ActionClass::Security,
        subjects: [SubjectKind::User, SubjectKind::Workload].into(),
        rule: vec![Condition::Granted],
        delegable: true,
    };
    ActionRegistry {
        version: "r1".to_owned(),
        actions: BTreeMap::from([(action.id.clone(), action)]),
    }
}

fn subject(kind: SubjectKind, n: u8) -> Subject {
    Subject {
        kind,
        id: Uuid::from_bytes([n; 16]),
    }
}

fn root(owner: Subject) -> Grant {
    Grant {
        id: Uuid::from_u128(1),
        subject: owner,
        issuer: subject(SubjectKind::Service, 9),
        actions: ["network.egress".to_owned()].into(),
        resource: exact("network", "api.example"),
        delegable_actions: ["network.egress".to_owned()].into(),
        max_depth: 3,
        parent: None,
        not_before_millis: NOW - 1_000,
        expires_at_millis: Some(NOW + 60_000),
        revoked_at_millis: None,
        policy_version: "p1".to_owned(),
    }
}

fn child_request(id: u128, to: Subject) -> DelegationRequest {
    DelegationRequest {
        id: Uuid::from_u128(id),
        subject: to,
        actions: ["network.egress".to_owned()].into(),
        resource: exact("network", "api.example"),
        delegable_actions: ["network.egress".to_owned()].into(),
        max_depth: 3,
        not_before_millis: NOW - 1_000,
        expires_at_millis: None,
    }
}

#[test]
fn issued_and_delegated_grants_authorize_until_an_ancestor_is_revoked() {
    let world = World::default();
    let registry = registry();
    let owner = subject(SubjectKind::User, 1);
    let workload = subject(SubjectKind::Workload, 2);
    let child_workload = subject(SubjectKind::Workload, 3);
    IssueGrant {
        grants: &world,
        registry: &registry,
        clock: &world,
    }
    .execute(&root(owner))
    .unwrap();
    let delegate = DelegateGrant {
        grants: &world,
        registry: &registry,
        clock: &world,
    };
    let child = delegate
        .execute(
            &owner,
            Uuid::from_u128(1),
            &child_request(2, workload),
            "p1",
        )
        .unwrap();
    let grandchild = delegate
        .execute(&workload, child.id, &child_request(3, child_workload), "p1")
        .unwrap();
    // Delegation narrowed time and depth to the parent's.
    assert_eq!(child.expires_at_millis, Some(NOW + 60_000));
    assert_eq!((child.max_depth, grandchild.max_depth), (2, 1));

    let policy = Registry(registry.clone());
    let authorize = Authorize {
        policy: &policy,
        grants: &world,
        clock: &world,
    };
    let decide = |who: Subject, host: &str| {
        authorize
            .decide(
                Some(who),
                "network.egress",
                ResourceRef {
                    kind: "network".to_owned(),
                    id: host.to_owned(),
                },
                BTreeSet::new(),
            )
            .unwrap()
    };
    let allowed = decide(child_workload, "api.example");
    assert!(allowed.allowed);
    assert_eq!(allowed.matched, Some(Condition::Granted));
    assert_eq!(
        allowed.grant_chain,
        vec![grandchild.id, child.id, Uuid::from_u128(1)]
    );
    assert_eq!(
        decide(child_workload, "evil.example").reason,
        DecisionReason::ConditionNotMet
    );

    // Revoking the root voids the whole subtree and reports it.
    let closure = RevokeGrant {
        grants: &world,
        clock: &world,
    }
    .execute(Uuid::from_u128(1))
    .unwrap();
    assert_eq!(closure, vec![Uuid::from_u128(1), child.id, grandchild.id]);
    assert!(!decide(child_workload, "api.example").allowed);
    assert!(!decide(workload, "api.example").allowed);
    assert!(!grant_is_live(&world, grandchild.id, NOW).unwrap());
    assert!(matches!(
        RevokeGrant {
            grants: &world,
            clock: &world,
        }
        .execute(Uuid::from_u128(99)),
        Err(ApplicationError::Denied(_))
    ));
}

#[test]
fn issuance_follows_the_registry_and_delegation_needs_the_holder() {
    let world = World::default();
    let registry = registry();
    let issue = IssueGrant {
        grants: &world,
        registry: &registry,
        clock: &world,
    };
    let owner = subject(SubjectKind::User, 1);
    let mut node_grant = root(subject(SubjectKind::Node, 4));
    node_grant.id = Uuid::from_u128(40);
    assert_eq!(
        issue.execute(&node_grant),
        Err(ApplicationError::Denied(
            "the subject class may not hold the action".to_owned()
        ))
    );
    let mut wrong_resource = root(owner);
    wrong_resource.resource = exact("store", "s1");
    assert!(issue.execute(&wrong_resource).is_err());
    let mut expired = root(owner);
    expired.expires_at_millis = Some(NOW);
    assert!(issue.execute(&expired).is_err());
    issue.execute(&root(owner)).unwrap();
    assert_eq!(
        issue.execute(&root(owner)),
        Err(ApplicationError::Denied(
            "the grant already exists".to_owned()
        ))
    );
    // Someone who does not hold the parent cannot delegate from it.
    assert_eq!(
        DelegateGrant {
            grants: &world,
            registry: &registry,
            clock: &world,
        }
        .execute(
            &subject(SubjectKind::User, 7),
            Uuid::from_u128(1),
            &child_request(5, subject(SubjectKind::Workload, 5)),
            "p1",
        ),
        Err(ApplicationError::Denied(
            "the delegator does not hold the parent grant".to_owned()
        ))
    );
}
