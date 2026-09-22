use super::*;
use DesiredWorkloadState as D;
use ObservedWorkloadState as O;

fn generation(value: u64) -> Generation {
    Generation::from_stored(value).unwrap()
}

fn observed(state: O, generation_value: u64, sequence: u64) -> Option<Observation> {
    Some(Observation {
        state,
        generation: generation(generation_value),
        sequence,
        reason: None,
        observed_at_millis: 0,
    })
}

#[test]
fn desired_transitions_follow_the_state_table() {
    assert_eq!(desired_transition(D::Stopped, D::Running), Ok(()));
    assert_eq!(desired_transition(D::Running, D::Paused), Ok(()));
    assert_eq!(desired_transition(D::Paused, D::Running), Ok(()));
    assert_eq!(desired_transition(D::Running, D::Running), Ok(()));
    assert_eq!(
        desired_transition(D::Stopped, D::Paused),
        Err(LifecycleError::InvalidTransition)
    );
    for to in [D::Running, D::Stopped, D::Deleted] {
        assert_eq!(
            desired_transition(D::Deleted, to),
            Err(LifecycleError::Deleted)
        );
    }
    let table = desired_transitions();
    assert!(table.contains(&(D::Paused, D::Deleted)));
    assert!(!table.contains(&(D::Stopped, D::Paused)));
    assert!(table.iter().all(|(from, _)| *from != D::Deleted));
}

#[test]
fn desired_changes_are_compare_and_set_on_the_generation() {
    assert_eq!(
        next_desired(D::Stopped, generation(3), generation(3), D::Running),
        Ok(generation(4))
    );
    assert_eq!(
        next_desired(D::Stopped, generation(3), generation(2), D::Running),
        Err(LifecycleError::StaleGeneration)
    );
    assert_eq!(
        next_desired(D::Deleted, generation(3), generation(3), D::Running),
        Err(LifecycleError::Deleted)
    );
}

#[test]
fn observations_never_go_back_or_ahead_of_desired() {
    let recorded = observed(O::Running, 2, 5);
    assert_eq!(
        accept_observation(
            generation(3),
            recorded.as_ref(),
            &observed(O::Stopped, 3, 1).unwrap()
        ),
        Ok(())
    );
    assert_eq!(
        accept_observation(
            generation(3),
            recorded.as_ref(),
            &observed(O::Failed, 2, 6).unwrap()
        ),
        Ok(())
    );
    assert_eq!(
        accept_observation(
            generation(3),
            recorded.as_ref(),
            &observed(O::Stopped, 2, 5).unwrap()
        ),
        Err(LifecycleError::StaleObservation)
    );
    assert_eq!(
        accept_observation(
            generation(3),
            recorded.as_ref(),
            &observed(O::Stopped, 1, 9).unwrap()
        ),
        Err(LifecycleError::StaleObservation)
    );
    assert_eq!(
        accept_observation(generation(3), None, &observed(O::Running, 4, 1).unwrap()),
        Err(LifecycleError::FutureObservation)
    );
}

#[test]
fn reconciliation_takes_one_step_toward_desired() {
    let running = Some((D::Running, generation(2)));
    assert_eq!(
        reconcile(running, observed(O::Running, 2, 1).as_ref()),
        ReconcileAction::None
    );
    // Running at an older generation: the new desired generation must be applied.
    assert_eq!(
        reconcile(running, observed(O::Running, 1, 1).as_ref()),
        ReconcileAction::Start
    );
    assert_eq!(reconcile(running, None), ReconcileAction::Start);
    assert_eq!(
        reconcile(running, observed(O::Lost, 2, 1).as_ref()),
        ReconcileAction::Restart
    );
    assert_eq!(
        reconcile(running, observed(O::Paused, 2, 1).as_ref()),
        ReconcileAction::Resume
    );
    let paused = Some((D::Paused, generation(3)));
    assert_eq!(
        reconcile(paused, observed(O::Running, 2, 1).as_ref()),
        ReconcileAction::Pause
    );
    let stopped = Some((D::Stopped, generation(4)));
    assert_eq!(
        reconcile(stopped, observed(O::Running, 3, 1).as_ref()),
        ReconcileAction::Stop
    );
    assert_eq!(reconcile(stopped, None), ReconcileAction::None);
    let deleted = Some((D::Deleted, generation(5)));
    assert_eq!(
        reconcile(deleted, observed(O::Stopped, 4, 1).as_ref()),
        ReconcileAction::Delete
    );
    assert_eq!(reconcile(deleted, None), ReconcileAction::None);
    assert_eq!(
        reconcile(deleted, observed(O::Stopped, 5, 2).as_ref()),
        ReconcileAction::None
    );
    assert_eq!(
        reconcile(deleted, observed(O::Running, 5, 2).as_ref()),
        ReconcileAction::Delete
    );
    // An instance nobody desired is adopted for review, never run or deleted silently.
    assert_eq!(
        reconcile(None, observed(O::Running, 1, 1).as_ref()),
        ReconcileAction::Adopt
    );
    assert_eq!(reconcile(None, None), ReconcileAction::None);
}

#[test]
fn operations_finish_once_and_capabilities_refuse_the_unsupported() {
    use OperationState as S;
    assert_eq!(operation_transition(S::Pending, S::Running), Ok(()));
    assert_eq!(operation_transition(S::Running, S::Succeeded), Ok(()));
    assert_eq!(operation_transition(S::Pending, S::Cancelled), Ok(()));
    assert_eq!(
        operation_transition(S::Succeeded, S::Failed),
        Err(LifecycleError::OperationFinished)
    );
    assert_eq!(
        operation_transition(S::Running, S::Pending),
        Err(LifecycleError::InvalidTransition)
    );
    let wasm = RuntimeCapabilities {
        runtime: "wasm".to_owned(),
        invocation: true,
        ..RuntimeCapabilities::default()
    };
    assert_eq!(wasm.check(WorkloadOperation::Invoke), Ok(()));
    assert_eq!(
        wasm.check(WorkloadOperation::Pause),
        Err(LifecycleError::Unsupported)
    );
}

#[test]
fn the_node_generation_orders_commands() {
    assert_eq!(
        command_freshness(None, generation(1)),
        CommandFreshness::Apply
    );
    assert_eq!(
        command_freshness(Some(generation(2)), generation(3)),
        CommandFreshness::Apply
    );
    assert_eq!(
        command_freshness(Some(generation(2)), generation(2)),
        CommandFreshness::Replay
    );
    assert_eq!(
        command_freshness(Some(generation(2)), generation(1)),
        CommandFreshness::Stale
    );
}
