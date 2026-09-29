//! A607 coordination on the storage module, on every provider: the fenced lease
//! passes its port conformance suite, replicas racing for one lease produce exactly
//! one holder, and a paused holder's effect is refused at the destination.

mod support;

use std::sync::{Arc, Barrier};

use aseman_capsule::coordination::StorageCoordination;
use aseman_domain::coordination::{Acquisition, LeaseName};
use aseman_ports::conformance::coordination::{check_coordination, check_fenced_destination};
use aseman_ports::coordination::CoordinationPort;

#[test]
fn coordination_fences_singleton_work() {
    for store in support::stores() {
        eprintln!("coordination on {}", store.name);
        let coordination = StorageCoordination::new(store.storage.clone());
        check_coordination(&coordination, "conformance", |millis| {
            std::thread::sleep(std::time::Duration::from_millis(millis));
        });
        check_fenced_destination(&coordination, "conformance-fence");

        // Eight replicas race for one lease. Exactly one may win, and the losers must be
        // told who holds it rather than being handed a second lease.
        let name = LeaseName::new("race").unwrap();
        let coordination = Arc::new(coordination);
        let barrier = Arc::new(Barrier::new(8));
        let winners: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|replica| {
                    let coordination = Arc::clone(&coordination);
                    let barrier = Arc::clone(&barrier);
                    let name = name.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        coordination
                            .acquire(&name, &format!("replica-{replica}"), 60_000)
                            .unwrap()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect()
        });
        let granted: Vec<_> = winners
            .iter()
            .filter_map(|outcome| match outcome {
                Acquisition::Granted(lease) => Some(lease),
                Acquisition::Held { .. } => None,
            })
            .collect();
        assert_eq!(
            granted.len(),
            1,
            "exactly one replica may hold a lease: {winners:?}"
        );
        let holder = granted[0];
        for outcome in &winners {
            if let Acquisition::Held { holder: named, .. } = outcome {
                assert_eq!(
                    *named, holder.instance,
                    "a loser is told who actually holds the lease"
                );
            }
        }

        // The holder's effects are accepted; a fenced-out replica's are not.
        use aseman_ports::coordination::FencedDestination;
        coordination.accept(&name, holder.token).unwrap();
        coordination.release(holder).unwrap();
        let next = match coordination.acquire(&name, "replica-next", 60_000).unwrap() {
            Acquisition::Granted(lease) => lease,
            other => panic!("a released lease is free: {other:?}"),
        };
        assert!(next.token > holder.token);
        coordination.accept(&name, next.token).unwrap();
        assert_eq!(
            coordination.accept(&name, holder.token),
            Err(aseman_ports::PortError::Conflict),
            "the paused former holder cannot commit behind the new one"
        );
    }
}

/// The application's singleton runner over each provider: several replicas run the
/// same worker loop, and the work happens on one of them at a time.
#[test]
fn singleton_work_happens_on_one_replica_at_a_time() {
    for store in support::stores() {
        eprintln!("singleton on {}", store.name);
        let coordination = StorageCoordination::new(store.storage.clone());
        use aseman_application::singleton::{Pass, Singleton};
        use aseman_domain::coordination::SafetyMargin;

        let name = LeaseName::new("worker").unwrap();
        let margin = SafetyMargin::new(1_000).unwrap();
        // Who was inside the work, and when: overlapping entries would be two replicas
        // doing singleton work at once.
        let inside = Arc::new(std::sync::Mutex::new(Vec::<(String, bool)>::new()));

        std::thread::scope(|scope| {
            for replica in 0..4 {
                let coordination = &coordination;
                let name = name.clone();
                let inside = Arc::clone(&inside);
                scope.spawn(move || {
                    let instance = format!("replica-{replica}");
                    let mut singleton =
                        Singleton::new(coordination, name, instance.clone(), 5_000, margin);
                    for _ in 0..10 {
                        let outcome = singleton.run(|_token| {
                            inside.lock().unwrap().push((instance.clone(), true));
                            std::thread::sleep(std::time::Duration::from_millis(20));
                            inside.lock().unwrap().push((instance.clone(), false));
                        });
                        assert!(
                            !matches!(outcome, Ok((Pass::Yielded, _))),
                            "a lease this short-lived should not be lost mid-run"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(30));
                    }
                    singleton.resign().unwrap();
                });
            }
        });

        // Replay the enter/leave log: the number of replicas inside the work never
        // exceeds one.
        let log = inside.lock().unwrap().clone();
        assert!(!log.is_empty(), "the work ran at least once");
        let mut depth = 0i32;
        for (instance, entering) in &log {
            depth += if *entering { 1 } else { -1 };
            assert!(
                depth <= 1,
                "two replicas were inside the singleton work at once, at {instance}"
            );
        }
        assert_eq!(depth, 0, "every entry has its exit");
    }
}
