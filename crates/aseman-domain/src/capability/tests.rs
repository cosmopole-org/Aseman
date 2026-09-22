use super::*;
use crate::authority::{ActionClass, Condition, RegisteredAction};
use crate::identity::SubjectKind;

const NOW: i64 = 1_800_000_000_000;

fn subject(kind: SubjectKind, n: u8) -> Subject {
    Subject {
        kind,
        id: Uuid::from_bytes([n; 16]),
    }
}

fn registry() -> ActionRegistry {
    let action = |id: &str, resource: &str, delegable: bool| RegisteredAction {
        id: id.to_owned(),
        resource: resource.to_owned(),
        class: ActionClass::Write,
        subjects: [
            SubjectKind::User,
            SubjectKind::Creature,
            SubjectKind::Workload,
        ]
        .into_iter()
        .collect(),
        rule: vec![Condition::Owner, Condition::Granted],
        delegable,
    };
    ActionRegistry {
        version: "test".to_owned(),
        actions: [
            action("store.signal", "store", true),
            action("store.read", "store", true),
            action("store.access.write", "store", false),
            action("network.egress", "network", true),
        ]
        .into_iter()
        .map(|action| (action.id.clone(), action))
        .collect(),
    }
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

fn store(id: &str) -> ResourceSelector {
    ResourceSelector::Exact {
        kind: "store".to_owned(),
        id: id.to_owned(),
    }
}

fn root() -> Grant {
    Grant {
        id: Uuid::from_bytes([1; 16]),
        subject: subject(SubjectKind::User, 1),
        issuer: subject(SubjectKind::User, 9),
        actions: set(&["store.signal", "store.read", "store.access.write"]),
        resource: ResourceSelector::AnyOfKind {
            kind: "store".to_owned(),
        },
        delegable_actions: set(&["store.signal", "store.read", "store.access.write"]),
        max_depth: 2,
        parent: None,
        not_before_millis: NOW - 1_000,
        expires_at_millis: Some(NOW + 60_000),
        revoked_at_millis: None,
        policy_version: "p1".to_owned(),
    }
}

fn request(id: u8, subject_kind: SubjectKind) -> DelegationRequest {
    DelegationRequest {
        id: Uuid::from_bytes([id; 16]),
        subject: subject(subject_kind, id),
        actions: set(&["store.signal", "store.access.write", "network.egress"]),
        resource: store("s1"),
        delegable_actions: set(&["store.signal"]),
        max_depth: 9,
        not_before_millis: NOW - 5_000,
        expires_at_millis: Some(NOW + 600_000),
    }
}

#[test]
fn delegation_intersects_and_never_extends() {
    let registry = registry();
    let child = delegate(
        &registry,
        &[root()],
        &root().subject,
        &request(2, SubjectKind::Workload),
        "p1",
        NOW,
    )
    .unwrap();
    // Non-delegable (`store.access.write`) and foreign-resource (`network.egress`)
    // actions are dropped.
    assert_eq!(child.actions, set(&["store.signal"]));
    assert_eq!(child.delegable_actions, set(&["store.signal"]));
    assert_eq!(child.resource, store("s1"));
    assert_eq!(child.max_depth, 1);
    assert_eq!(child.expires_at_millis, Some(NOW + 60_000));
    assert_eq!(child.not_before_millis, NOW - 1_000);
    assert_eq!(child.issuer, root().subject);
    let chain = [child.clone(), root()];
    assert_eq!(check_chain(&chain, NOW), Ok(()));
    assert!(chain_authorizes(
        &chain,
        &child.subject,
        "store.signal",
        "store",
        "s1",
        NOW
    ));
    assert!(!chain_authorizes(
        &chain,
        &child.subject,
        "store.signal",
        "store",
        "s2",
        NOW
    ));
    assert!(!chain_authorizes(
        &chain,
        &child.subject,
        "store.read",
        "store",
        "s1",
        NOW
    ));
    // Only the holder delegates, and depth runs out.
    assert_eq!(
        delegate(
            &registry,
            &[root()],
            &subject(SubjectKind::User, 7),
            &request(3, SubjectKind::Workload),
            "p1",
            NOW
        ),
        Err(CapabilityError::NotHolder)
    );
    let grandchild = delegate(
        &registry,
        &chain,
        &child.subject,
        &request(4, SubjectKind::Workload),
        "p1",
        NOW,
    )
    .unwrap();
    assert_eq!(grandchild.max_depth, 0);
    let deep = [grandchild.clone(), child.clone(), root()];
    assert_eq!(check_chain(&deep, NOW), Ok(()));
    assert_eq!(
        delegate(
            &registry,
            &deep,
            &grandchild.subject,
            &request(5, SubjectKind::Workload),
            "p1",
            NOW
        ),
        Err(CapabilityError::DepthExhausted)
    );
    // Disjoint resources and empty intersections are refused.
    let mut elsewhere = request(6, SubjectKind::Workload);
    elsewhere.resource = ResourceSelector::AnyOfKind {
        kind: "network".to_owned(),
    };
    assert_eq!(
        delegate(&registry, &[root()], &root().subject, &elsewhere, "p1", NOW),
        Err(CapabilityError::ResourceOutside)
    );
    let mut nothing = request(7, SubjectKind::Workload);
    nothing.actions = set(&["store.access.write"]);
    assert_eq!(
        delegate(&registry, &[root()], &root().subject, &nothing, "p1", NOW),
        Err(CapabilityError::NothingDelegable)
    );
    assert_eq!(
        delegate(
            &registry,
            &[root()],
            &root().subject,
            &request(8, SubjectKind::Node),
            "p1",
            NOW
        ),
        Err(CapabilityError::SubjectNotAllowed)
    );
}

#[test]
fn revoking_or_expiring_an_ancestor_voids_descendants() {
    let registry = registry();
    let child = delegate(
        &registry,
        &[root()],
        &root().subject,
        &request(2, SubjectKind::Workload),
        "p1",
        NOW,
    )
    .unwrap();
    let mut revoked_root = root();
    revoked_root.revoked_at_millis = Some(NOW - 1);
    assert_eq!(
        check_chain(&[child.clone(), revoked_root], NOW),
        Err(CapabilityError::NotLive)
    );
    assert_eq!(
        check_chain(&[child.clone(), root()], NOW + 60_000),
        Err(CapabilityError::NotLive)
    );
    let grandchild = delegate(
        &registry,
        &[child.clone(), root()],
        &child.subject,
        &request(3, SubjectKind::Workload),
        "p1",
        NOW,
    )
    .unwrap();
    let unrelated = Grant {
        id: Uuid::from_bytes([40; 16]),
        parent: None,
        ..root()
    };
    let all = [root(), child.clone(), grandchild.clone(), unrelated];
    assert_eq!(
        revocation_closure(&all, root().id),
        BTreeSet::from([root().id, child.id, grandchild.id])
    );
    assert_eq!(
        revocation_closure(&all, child.id),
        BTreeSet::from([child.id, grandchild.id])
    );
}

#[test]
fn tampered_chains_are_refused() {
    let registry = registry();
    let child = delegate(
        &registry,
        &[root()],
        &root().subject,
        &request(2, SubjectKind::Workload),
        "p1",
        NOW,
    )
    .unwrap();
    // A grandchild claiming more than its narrower parent (one store, `store.signal`).
    let grandchild = delegate(
        &registry,
        &[child.clone(), root()],
        &child.subject,
        &request(3, SubjectKind::Workload),
        "p1",
        NOW,
    )
    .unwrap();
    let mut wider_resource = grandchild.clone();
    wider_resource.resource = ResourceSelector::AnyOfKind {
        kind: "store".to_owned(),
    };
    assert_eq!(
        check_chain(&[wider_resource, child.clone(), root()], NOW),
        Err(CapabilityError::Amplifies)
    );
    let mut more_actions = grandchild.clone();
    more_actions.actions.insert("store.read".to_owned());
    assert_eq!(
        check_chain(&[more_actions, child.clone(), root()], NOW),
        Err(CapabilityError::Amplifies)
    );
    let mut deeper = grandchild;
    deeper.max_depth = 5;
    assert_eq!(
        check_chain(&[deeper, child.clone(), root()], NOW),
        Err(CapabilityError::Amplifies)
    );
    let mut longer = child.clone();
    longer.expires_at_millis = None;
    assert_eq!(
        check_chain(&[longer, root()], NOW),
        Err(CapabilityError::Amplifies)
    );
    let mut forged_issuer = child.clone();
    forged_issuer.issuer = subject(SubjectKind::User, 7);
    assert_eq!(
        check_chain(&[forged_issuer, root()], NOW),
        Err(CapabilityError::Amplifies)
    );
    let mut orphan = child.clone();
    orphan.parent = Some(Uuid::from_bytes([77; 16]));
    assert_eq!(
        check_chain(&[orphan, root()], NOW),
        Err(CapabilityError::BrokenChain)
    );
    assert_eq!(check_chain(&[], NOW), Err(CapabilityError::BrokenChain));
    assert_eq!(
        check_chain(&[child], NOW),
        Err(CapabilityError::BrokenChain)
    );
}

/// A deterministic generator, so failures reproduce.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn pick<'a>(&mut self, items: &'a [&'a str]) -> Vec<&'a str> {
        items
            .iter()
            .copied()
            .filter(|_| self.next().is_multiple_of(2))
            .collect()
    }
}

#[test]
fn property_no_delegation_ever_amplifies_authority() {
    let registry = registry();
    let actions = [
        "store.signal",
        "store.read",
        "store.access.write",
        "network.egress",
    ];
    let mut random = Lcg(0x5eed);
    for round in 0..2_000u32 {
        let mut chain = vec![root()];
        for level in 0..4u8 {
            let holder = chain[0].subject;
            let request = DelegationRequest {
                id: Uuid::from_u128((u128::from(round) << 8) | (u128::from(level) + 2)),
                subject: subject(SubjectKind::Workload, level + 2),
                actions: set(&random.pick(&actions)),
                resource: match random.next() % 3 {
                    0 => ResourceSelector::AnyOfKind {
                        kind: "store".to_owned(),
                    },
                    1 => store("s1"),
                    _ => store("s2"),
                },
                delegable_actions: set(&random.pick(&actions)),
                max_depth: u32::try_from(random.next() % 5).unwrap(),
                not_before_millis: NOW - 10_000 + i64::try_from(random.next() % 20_000).unwrap(),
                expires_at_millis: match random.next() % 3 {
                    0 => None,
                    _ => Some(NOW + i64::try_from(random.next() % 200_000).unwrap()),
                },
            };
            let Ok(child) = delegate(&registry, &chain, &holder, &request, "p1", NOW) else {
                break;
            };
            let parent = &chain[0];
            assert!(child.actions.is_subset(&parent.delegable_actions));
            assert!(child.actions.is_subset(&request.actions));
            assert!(child.delegable_actions.is_subset(&child.actions));
            assert!(child.resource.within(&parent.resource));
            assert!(child.max_depth < parent.max_depth);
            assert!(child.not_before_millis >= parent.not_before_millis);
            if let Some(limit) = parent.expires_at_millis {
                assert!(
                    child
                        .expires_at_millis
                        .is_some_and(|expiry| expiry <= limit)
                );
            }
            for action in &child.actions {
                assert!(registry.actions[action].delegable);
            }
            chain.insert(0, child);
            // Whatever was produced verifies as a chain, at every instant it claims.
            let head = &chain[0];
            if head.not_before_millis <= NOW
                && head.expires_at_millis.is_none_or(|expiry| NOW < expiry)
            {
                assert_eq!(check_chain(&chain, NOW), Ok(()));
            }
        }
    }
}
