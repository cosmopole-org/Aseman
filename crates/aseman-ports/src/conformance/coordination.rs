//! The A607 coordination conformance kit.
//!
//! Every [`CoordinationPort`] implementation runs this. The cases are the ones that
//! decide whether singleton work can execute twice: a live lease is not stolen, an
//! expired one is taken over at a higher token, a renewal after takeover fails, and a
//! paused holder's effect is refused at the destination.

use aseman_domain::coordination::{
    Acquisition, FencingToken, LeaseName, LeaseStanding, SafetyMargin, may_commit, standing,
};

use crate::PortError;
use crate::coordination::{CoordinationPort, FencedDestination};

fn granted(outcome: Acquisition, what: &str) -> aseman_domain::coordination::Lease {
    match outcome {
        Acquisition::Granted(lease) => lease,
        Acquisition::Held {
            holder,
            expires_at_millis,
        } => panic!("{what}: held by {holder} until {expires_at_millis}"),
    }
}

/// Run the coordination contract against `port`, using a lease name no other test
/// uses. `sleep` must advance the provider's clock by at least the given milliseconds.
///
/// # Panics
///
/// Panics when the provider deviates from the port contract.
pub fn check_coordination(port: &dyn CoordinationPort, name: &str, sleep: impl Fn(u64)) {
    let name = LeaseName::new(name).expect("a lease name");

    // An unheld lease is free, and reads as absent.
    assert!(
        port.read(&name).expect("read").is_none(),
        "the suite needs a lease name nothing else holds"
    );

    // First acquisition: the first token.
    let first = granted(
        port.acquire(&name, "replica-a", 60_000).expect("acquire"),
        "a free lease",
    );
    assert_eq!(first.instance, "replica-a");
    assert_eq!(first.name, name);
    assert!(
        first.token >= FencingToken::FIRST,
        "a token starts at 1: {}",
        first.token
    );
    assert!(
        first.expires_at_millis > first.acquired_at_millis,
        "a lease expires after it is acquired"
    );

    // The provider's time is the holder's clock, and it is inside the lease.
    let now = port.now_millis().expect("now");
    assert!(
        now >= first.acquired_at_millis && now < first.expires_at_millis,
        "the provider's time is inside the lease it just granted"
    );

    // A live lease is not taken from its holder. This is the whole point.
    match port
        .acquire(&name, "replica-b", 60_000)
        .expect("a contested acquire")
    {
        Acquisition::Held { holder, .. } => assert_eq!(holder, "replica-a"),
        Acquisition::Granted(lease) => {
            panic!(
                "a live lease was stolen by {} at {}",
                lease.instance, lease.token
            )
        }
    }

    // Reading it back agrees with the holder.
    let read = port.read(&name).expect("read").expect("a held lease");
    assert_eq!(read.instance, first.instance);
    assert_eq!(read.token, first.token);

    // A renewal keeps the token: it is the same run of work.
    let renewed = port
        .renew(&first, 60_000)
        .expect("renew")
        .expect("the holder renews its own lease");
    assert_eq!(
        renewed.token, first.token,
        "a renewal never moves the token"
    );
    assert!(
        renewed.expires_at_millis >= first.expires_at_millis,
        "a renewal extends the lease"
    );

    // Releasing frees it for the next replica, which gets a higher token.
    port.release(&renewed).expect("release");
    assert!(
        port.read(&name).expect("read").is_none()
            || port
                .read(&name)
                .expect("read")
                .is_some_and(|lease| lease.expires_at_millis <= port.now_millis().expect("now")),
        "a released lease is free"
    );
    let second = granted(
        port.acquire(&name, "replica-b", 60_000).expect("acquire"),
        "a released lease",
    );
    assert_eq!(second.instance, "replica-b");
    assert!(
        second.token > first.token,
        "every acquisition allocates a higher token: {} then {}",
        first.token,
        second.token
    );

    // The old holder cannot renew after the takeover; it must stop, not retry.
    assert!(
        port.renew(&first, 60_000).expect("renew").is_none(),
        "a fenced-out holder does not get its lease back by renewing"
    );
    // Nor can its release disturb the new holder.
    port.release(&first).expect("a stale release is harmless");
    let still = port.read(&name).expect("read").expect("still held");
    assert_eq!(
        still.instance, "replica-b",
        "a stale release must not free the new holder's lease"
    );

    // A short lease really expires, and the next acquisition takes it over.
    port.release(&second).expect("release");
    let brief = granted(
        port.acquire(&name, "replica-c", 1_000).expect("acquire"),
        "a brief lease",
    );
    sleep(1_500);
    let after = granted(
        port.acquire(&name, "replica-d", 60_000)
            .expect("an expired lease is taken over"),
        "an expired lease",
    );
    assert_eq!(after.instance, "replica-d");
    assert!(after.token > brief.token);

    // The expired holder's standing is Stop, by the provider's clock.
    let margin = SafetyMargin::new(200).expect("margin");
    assert_eq!(
        standing(&brief, margin, port.now_millis().expect("now")),
        LeaseStanding::Stop,
        "an expired holder stops"
    );

    port.release(&after).expect("release");
}

/// Run the destination-side fencing contract: the guard that makes a lease mean
/// something when the effect lands somewhere else.
///
/// # Panics
///
/// Panics when the destination deviates from the port contract.
pub fn check_fenced_destination(destination: &dyn FencedDestination, name: &str) {
    let name = LeaseName::new(name).expect("a lease name");
    assert!(
        destination.last_accepted(&name).expect("read").is_none(),
        "the suite needs a name nothing has committed under"
    );

    let old = FencingToken::from_stored(7).expect("token");
    let new = FencingToken::from_stored(9).expect("token");

    assert!(may_commit(old, None), "nothing to fence against yet");
    destination.accept(&name, old).expect("the first effect");
    assert_eq!(destination.last_accepted(&name).expect("read"), Some(old));

    // One holder commits many effects under one token.
    destination.accept(&name, old).expect("a second effect");

    // The new holder moves the fence forward.
    destination.accept(&name, new).expect("the new holder");
    assert_eq!(destination.last_accepted(&name).expect("read"), Some(new));

    // The paused former holder wakes up and is refused.
    assert!(!may_commit(old, Some(new)));
    assert_eq!(
        destination.accept(&name, old),
        Err(PortError::Conflict),
        "a fenced-out holder's effect is refused, not silently dropped"
    );
    assert_eq!(
        destination.last_accepted(&name).expect("read"),
        Some(new),
        "a refused effect does not move the fence back"
    );
}
