use std::collections::BTreeMap;
use std::sync::Mutex;

use aseman_domain::coordination::plan_acquisition;
use aseman_ports::PortError;

use super::*;

/// A coordination provider with a clock the test moves by hand. It behaves exactly as
/// the port requires, so what the tests below show is the caller's behavior, not a
/// convenient fake's.
struct Clockwork {
    now: Mutex<i64>,
    leases: Mutex<BTreeMap<String, Lease>>,
    unreachable: Mutex<bool>,
}

impl Clockwork {
    fn new() -> Self {
        Self {
            now: Mutex::new(1_000),
            leases: Mutex::new(BTreeMap::new()),
            unreachable: Mutex::new(false),
        }
    }

    fn advance(&self, millis: i64) {
        *self.now.lock().expect("lock") += millis;
    }

    fn check(&self) -> PortResult<()> {
        if *self.unreachable.lock().expect("lock") {
            return Err(PortError::Unavailable("coordination"));
        }
        Ok(())
    }
}

impl CoordinationPort for Clockwork {
    fn acquire(
        &self,
        name: &LeaseName,
        instance: &str,
        ttl_millis: i64,
    ) -> PortResult<Acquisition> {
        self.check()?;
        let now = *self.now.lock().expect("lock");
        let mut leases = self.leases.lock().expect("lock");
        let planned = plan_acquisition(name, leases.get(name.as_str()), instance, now, ttl_millis)
            .map_err(|error| PortError::Failed(error.to_string()))?;
        if let Acquisition::Granted(lease) = &planned {
            leases.insert(name.as_str().to_owned(), lease.clone());
        }
        Ok(planned)
    }

    fn renew(&self, lease: &Lease, ttl_millis: i64) -> PortResult<Option<Lease>> {
        self.check()?;
        let now = *self.now.lock().expect("lock");
        let mut leases = self.leases.lock().expect("lock");
        let current = leases.get(lease.name.as_str());
        if !aseman_domain::coordination::may_renew(current, lease, now) {
            return Ok(None);
        }
        let renewed = Lease {
            expires_at_millis: now + ttl_millis,
            ..lease.clone()
        };
        leases.insert(lease.name.as_str().to_owned(), renewed.clone());
        Ok(Some(renewed))
    }

    fn release(&self, lease: &Lease) -> PortResult<()> {
        self.check()?;
        let now = *self.now.lock().expect("lock");
        let mut leases = self.leases.lock().expect("lock");
        if leases
            .get(lease.name.as_str())
            .is_some_and(|held| held.instance == lease.instance && held.token == lease.token)
        {
            // Expire it, never remove it: the row carries the token counter.
            leases.insert(
                lease.name.as_str().to_owned(),
                Lease {
                    expires_at_millis: now.max(lease.acquired_at_millis),
                    ..lease.clone()
                },
            );
        }
        Ok(())
    }

    fn read(&self, name: &LeaseName) -> PortResult<Option<Lease>> {
        self.check()?;
        Ok(self
            .leases
            .lock()
            .expect("lock")
            .get(name.as_str())
            .cloned())
    }

    fn now_millis(&self) -> PortResult<i64> {
        self.check()?;
        Ok(*self.now.lock().expect("lock"))
    }
}

fn singleton<'a>(port: &'a Clockwork, instance: &str) -> Singleton<'a> {
    Singleton::new(
        port,
        LeaseName::new("reconcile").expect("name"),
        instance,
        30_000,
        SafetyMargin::new(5_000).expect("margin"),
    )
}

#[test]
fn one_replica_works_and_the_others_stand_by() {
    let port = Clockwork::new();
    let mut first = singleton(&port, "replica-a");
    let mut second = singleton(&port, "replica-b");

    let (pass, ran) = first.run(|_| "work").expect("pass");
    assert!(matches!(pass, Pass::Ran(_)));
    assert_eq!(ran, Some("work"));

    let (pass, ran) = second.run(|_| "work").expect("pass");
    assert_eq!(
        pass,
        Pass::Standby {
            holder: "replica-a".to_owned()
        }
    );
    assert_eq!(ran, None, "a standby replica does not do the work");
}

#[test]
fn the_holder_keeps_working_and_renews_itself() {
    let port = Clockwork::new();
    let mut holder = singleton(&port, "replica-a");
    let (first, _) = holder.run(|token| token).expect("pass");
    let Pass::Ran(token) = first else {
        panic!("the first pass runs");
    };

    // Comfortably inside the lease: no renewal needed, same token.
    port.advance(1_000);
    assert_eq!(holder.run(|token| token).expect("pass").0, Pass::Ran(token));

    // Past halfway to the stop deadline: it renews, and keeps its token.
    port.advance(12_000);
    assert_eq!(holder.run(|token| token).expect("pass").0, Pass::Ran(token));
    assert!(
        holder.lease().expect("held").expires_at_millis > 14_000,
        "the renewal extended the lease"
    );
}

#[test]
fn a_replica_inside_the_safety_margin_stops_before_the_lease_expires() {
    let port = Clockwork::new();
    let mut holder = singleton(&port, "replica-a");
    holder.run(|_| ()).expect("pass");
    let expires = holder.lease().expect("held").expires_at_millis;

    // Jump to a moment that is still inside the lease but inside the margin. The
    // replica must stop here, while the lease it believes in has not expired.
    *port.now.lock().expect("lock") = expires - 1_000;
    let (pass, ran) = holder.run(|_| "work").expect("pass");
    assert_eq!(pass, Pass::Yielded);
    assert_eq!(ran, None, "no effect is committed inside the margin");
    assert!(holder.lease().is_none());
}

#[test]
fn a_taken_over_replica_yields_without_doing_the_work() {
    let port = Clockwork::new();
    let mut first = singleton(&port, "replica-a");
    first.run(|_| ()).expect("pass");
    let held = first.lease().expect("held").clone();

    // The lease expires and another replica takes it, while the first replica is
    // paused and still believes it holds one.
    port.advance(31_000);
    let mut second = singleton(&port, "replica-b");
    let (pass, _) = second.run(|_| ()).expect("pass");
    let Pass::Ran(new_token) = pass else {
        panic!("the second replica takes the expired lease");
    };
    assert!(new_token > held.token, "the takeover moves the token");

    // The first replica wakes up. It must not work, and must not get its lease back.
    let (pass, ran) = first.run(|_| "work").expect("pass");
    assert_eq!(pass, Pass::Yielded);
    assert_eq!(ran, None);
    assert!(first.lease().is_none());
}

#[test]
fn a_replica_that_cannot_reach_the_provider_gives_up_its_lease() {
    let port = Clockwork::new();
    let mut holder = singleton(&port, "replica-a");
    holder.run(|_| ()).expect("pass");
    assert!(holder.lease().is_some());

    *port.unreachable.lock().expect("lock") = true;
    let error = holder.run(|_| "work").expect_err("the provider is gone");
    assert_eq!(error, PortError::Unavailable("coordination"));
    assert!(
        holder.lease().is_none(),
        "no fresh time means no lease: it may not keep working on an assumption"
    );
}

#[test]
fn resigning_hands_the_lease_over_without_waiting_for_expiry() {
    let port = Clockwork::new();
    let mut first = singleton(&port, "replica-a");
    first.run(|_| ()).expect("pass");
    first.resign().expect("resign");
    assert!(first.lease().is_none());

    let mut second = singleton(&port, "replica-b");
    let (pass, _) = second.run(|_| ()).expect("pass");
    assert!(
        matches!(pass, Pass::Ran(_)),
        "a resigned lease is free at once: {pass:?}"
    );
}
