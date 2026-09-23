use super::*;

fn done() -> StageOutcome {
    StageOutcome::Done
}

fn failed(reason: &str) -> StageOutcome {
    StageOutcome::Failed {
        reason: reason.to_owned(),
    }
}

#[test]
fn a_clean_host_runs_every_stage_in_order() {
    let mut progress = Progress::default();
    assert_eq!(progress.next(), Some(Stage::Preflight));
    for stage in Stage::ALL {
        assert_eq!(progress.next(), Some(stage));
        progress.record(stage, &done()).expect("recorded");
    }
    assert!(progress.complete());
    assert_eq!(progress.next(), None);
}

#[test]
fn an_interrupted_bootstrap_resumes_where_it_stopped() {
    let mut progress = Progress::default();
    for stage in [Stage::Preflight, Stage::Topology, Stage::Artifacts] {
        progress.record(stage, &done()).expect("recorded");
    }
    // The process died here. Re-entering from the beginning skips what was done.
    assert_eq!(progress.next(), Some(Stage::Identity));
    assert!(progress.is_done(Stage::Artifacts));
}

#[test]
fn a_finished_stage_never_runs_again() {
    let mut progress = Progress::default();
    progress
        .record(Stage::Preflight, &done())
        .expect("recorded");
    assert_eq!(
        progress.record(Stage::Preflight, &done()),
        Err(BootstrapError::AlreadyDone(Stage::Preflight)),
        "re-running a finished stage is how an installer destroys a working deployment"
    );
}

#[test]
fn a_stage_cannot_jump_ahead_of_what_it_depends_on() {
    let mut progress = Progress::default();
    assert_eq!(
        progress.record(Stage::Services, &done()),
        Err(BootstrapError::OutOfOrder(Stage::Services)),
        "starting services before the schema exists is not a shortcut"
    );
}

#[test]
fn a_failed_stage_is_retried_not_skipped() {
    let mut progress = Progress::default();
    progress
        .record(Stage::Preflight, &done())
        .expect("recorded");
    progress
        .record(Stage::Topology, &failed("no profile chosen"))
        .expect("recorded");
    assert_eq!(
        progress.next(),
        Some(Stage::Topology),
        "a failure leaves the stage still to do"
    );
    assert_eq!(
        progress.failed,
        Some((Stage::Topology, "no profile chosen".to_owned()))
    );
    progress.record(Stage::Topology, &done()).expect("recorded");
    assert_eq!(progress.failed, None, "a success clears the failure");
    assert_eq!(progress.next(), Some(Stage::Artifacts));
}

#[test]
fn rolling_back_never_destroys_working_data() {
    // Before the schema exists, a rollback removes what the bootstrap itself made.
    for stage in [
        Stage::Preflight,
        Stage::Topology,
        Stage::Artifacts,
        Stage::Identity,
    ] {
        assert_eq!(rollback(stage), Rollback::Undo, "{stage:?}");
    }
    // At and after it, undoing would drop a database or stop a running node.
    for stage in [Stage::Schema, Stage::Services, Stage::Health] {
        assert_eq!(
            rollback(stage),
            Rollback::RollForward,
            "{stage:?} is re-run, never undone"
        );
    }
}

#[test]
fn a_host_that_cannot_run_microvms_can_still_run_containers() {
    let findings = vec![
        Finding {
            check: "kvm".to_owned(),
            fatal: false,
            detail: "/dev/kvm is absent; microVM runtimes are unavailable".to_owned(),
        },
        Finding {
            check: "disk".to_owned(),
            fatal: false,
            detail: "less than 100 GiB free".to_owned(),
        },
    ];
    assert!(
        preflight_passed(&findings),
        "refusing to install because a host cannot also run microVMs would be wrong"
    );

    let mut fatal = findings;
    fatal.push(Finding {
        check: "cgroups".to_owned(),
        fatal: true,
        detail: "cgroup v2 is required".to_owned(),
    });
    assert!(!preflight_passed(&fatal));
}
