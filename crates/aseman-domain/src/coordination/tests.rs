use super::*;

fn name() -> LeaseName {
    LeaseName::new("reconcile").expect("name")
}

fn lease(instance: &str, token: u64, acquired: i64, expires: i64) -> Lease {
    Lease {
        name: name(),
        instance: instance.to_owned(),
        token: FencingToken::from_stored(token).expect("token"),
        acquired_at_millis: acquired,
        expires_at_millis: expires,
    }
}

#[test]
fn a_token_is_never_zero_and_always_moves_forward() {
    assert!(FencingToken::from_stored(0).is_err());
    let first = FencingToken::FIRST;
    assert_eq!(first.get(), 1);
    assert_eq!(first.next().expect("next").get(), 2);
    assert!(
        FencingToken::from_stored(u64::MAX)
            .expect("max")
            .next()
            .is_err()
    );
}

#[test]
fn a_free_lease_is_granted_at_the_first_token() {
    let plan = plan_acquisition(&name(), None, "replica-a", 1_000, 30_000).expect("plan");
    let Acquisition::Granted(lease) = plan else {
        panic!("a free lease is granted");
    };
    assert_eq!(lease.token, FencingToken::FIRST);
    assert_eq!(lease.instance, "replica-a");
    assert_eq!(lease.expires_at_millis, 31_000);
}

#[test]
fn a_live_lease_is_not_taken_from_its_holder() {
    let current = lease("replica-a", 4, 0, 30_000);
    let plan =
        plan_acquisition(&name(), Some(&current), "replica-b", 10_000, 30_000).expect("plan");
    assert_eq!(
        plan,
        Acquisition::Held {
            holder: "replica-a".to_owned(),
            expires_at_millis: 30_000,
        },
        "taking a live lease early is the double execution this prevents"
    );
}

#[test]
fn an_expired_lease_is_taken_over_at_a_higher_token() {
    let current = lease("replica-a", 4, 0, 30_000);
    let plan =
        plan_acquisition(&name(), Some(&current), "replica-b", 30_001, 30_000).expect("plan");
    let Acquisition::Granted(lease) = plan else {
        panic!("an expired lease is granted");
    };
    assert_eq!(lease.instance, "replica-b");
    assert_eq!(
        lease.token.get(),
        5,
        "the new holder fences the old one out"
    );
}

#[test]
fn reacquiring_your_own_lease_still_moves_the_token() {
    let current = lease("replica-a", 4, 0, 30_000);
    // The same instance re-acquires while it still holds the lease: the run of work
    // is new, so the token is new. Otherwise a gap in which another replica held and
    // released the lease would be invisible to a destination-side guard.
    let plan =
        plan_acquisition(&name(), Some(&current), "replica-a", 10_000, 30_000).expect("plan");
    let Acquisition::Granted(lease) = plan else {
        panic!("granted");
    };
    assert_eq!(lease.token.get(), 5);
}

#[test]
fn a_time_to_live_must_be_positive() {
    assert_eq!(
        plan_acquisition(&name(), None, "replica-a", 0, 0),
        Err(CoordinationError::InvalidTtl)
    );
}

#[test]
fn a_holder_stops_before_expiry_by_the_safety_margin() {
    let held = lease("replica-a", 1, 0, 30_000);
    let margin = SafetyMargin::new(5_000).expect("margin");
    assert_eq!(standing(&held, margin, 0), LeaseStanding::Held);
    assert_eq!(standing(&held, margin, 12_400), LeaseStanding::Held);
    assert_eq!(
        standing(&held, margin, 12_500),
        LeaseStanding::Renew,
        "renewal starts halfway to the stop deadline"
    );
    assert_eq!(
        standing(&held, margin, 25_000),
        LeaseStanding::Stop,
        "the holder stops a full margin before the lease expires"
    );
    assert_eq!(standing(&held, margin, 29_999), LeaseStanding::Stop);
    assert_eq!(standing(&held, margin, 30_001), LeaseStanding::Stop);
}

#[test]
fn a_margin_must_be_positive() {
    assert_eq!(SafetyMargin::new(0), Err(CoordinationError::InvalidMargin));
    assert_eq!(SafetyMargin::new(-1), Err(CoordinationError::InvalidMargin));
}

#[test]
fn a_paused_holder_cannot_commit_behind_the_new_one() {
    let old = FencingToken::from_stored(4).expect("token");
    let new = FencingToken::from_stored(5).expect("token");
    assert!(may_commit(old, None), "a first effect has nothing to fence");
    assert!(
        may_commit(new, Some(new)),
        "one holder commits many effects"
    );
    assert!(may_commit(new, Some(old)));
    assert!(
        !may_commit(old, Some(new)),
        "the woken former holder is refused"
    );
}

#[test]
fn a_renewal_needs_the_same_holder_and_token() {
    let held = lease("replica-a", 4, 0, 30_000);
    assert!(may_renew(Some(&held), &held, 10_000));
    assert!(
        !may_renew(Some(&held), &held, 30_001),
        "an expired lease is not renewed, it is re-acquired"
    );
    assert!(
        !may_renew(Some(&lease("replica-b", 5, 0, 30_000)), &held, 10_000),
        "a taken-over lease is not renewable"
    );
    assert!(
        !may_renew(Some(&lease("replica-a", 5, 0, 30_000)), &held, 10_000),
        "the token moved, so this is another run of work"
    );
    assert!(!may_renew(None, &held, 10_000), "a deleted lease is gone");
}

#[test]
fn a_name_is_bounded() {
    assert!(LeaseName::new("").is_err());
    assert!(LeaseName::new("x".repeat(129)).is_err());
    assert_eq!(LeaseName::new("outbox").expect("name").as_str(), "outbox");
}
