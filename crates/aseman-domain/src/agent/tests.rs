use super::*;

fn grant(operations: &[AgentOperation]) -> Grant {
    Grant {
        allocation: "alloc-one".to_owned(),
        profile: "small".to_owned(),
        expires_at_millis: 10_000,
        operations: operations.iter().copied().collect(),
    }
}

#[test]
fn a_grant_covers_one_allocation_and_no_other() {
    let grant = grant(&[AgentOperation::Start]);
    assert!(authorize(&grant, "alloc-one", AgentOperation::Start, 0).is_ok());
    assert_eq!(
        authorize(&grant, "alloc-two", AgentOperation::Start, 0),
        Err(AgentError::WrongAllocation),
        "a grant for one allocation is not a grant for the cluster"
    );
}

#[test]
fn an_expired_grant_is_refused_with_no_tolerance() {
    let grant = grant(&[AgentOperation::Start]);
    assert!(authorize(&grant, "alloc-one", AgentOperation::Start, 9_999).is_ok());
    assert_eq!(
        authorize(&grant, "alloc-one", AgentOperation::Start, 10_000),
        Err(AgentError::Expired)
    );
}

#[test]
fn a_grant_allows_only_what_it_names() {
    let grant = grant(&[AgentOperation::State]);
    assert!(authorize(&grant, "alloc-one", AgentOperation::State, 0).is_ok());
    assert_eq!(
        authorize(&grant, "alloc-one", AgentOperation::Delete, 0),
        Err(AgentError::NotGranted),
        "reading a machine's state is not permission to destroy it"
    );
}

#[test]
fn a_pause_is_the_runtimes_pause_and_never_a_stop() {
    assert!(MachineState::Running.allows(AgentOperation::Pause));
    assert!(
        !MachineState::Created.allows(AgentOperation::Pause),
        "a machine that never booted cannot be paused"
    );
    assert!(
        !MachineState::Stopped.allows(AgentOperation::Pause),
        "pausing a stopped machine must not silently succeed"
    );
    assert!(MachineState::Paused.allows(AgentOperation::Resume));
    assert!(
        !MachineState::Running.allows(AgentOperation::Resume),
        "resuming a running machine is a caller's mistake, not a no-op"
    );
}

#[test]
fn a_machine_can_always_be_looked_at_and_taken_away() {
    for state in [
        MachineState::Created,
        MachineState::Running,
        MachineState::Paused,
        MachineState::Stopped,
        MachineState::Failed,
    ] {
        assert!(state.allows(AgentOperation::State));
        assert!(state.allows(AgentOperation::Stop));
        assert!(state.allows(AgentOperation::Delete));
        assert!(
            !state.allows(AgentOperation::Create),
            "create is for an allocation with no machine yet"
        );
    }
}

#[test]
fn a_failed_machine_is_not_started_again_by_accident() {
    assert!(
        !MachineState::Failed.allows(AgentOperation::Start),
        "a machine whose process died is looked at, not silently restarted"
    );
    assert!(MachineState::Stopped.allows(AgentOperation::Start));
}

#[test]
fn an_allocation_name_that_could_escape_the_root_is_refused() {
    let root = Path::new("/var/lib/aseman-agent");
    assert_eq!(
        allocation_directory(root, "alloc-one").expect("a plain name"),
        root.join("alloc-one")
    );
    for hostile in [
        "..",
        "../etc",
        "alloc/../../etc",
        "/etc/shadow",
        "alloc one",
        "",
        "a.b",
    ] {
        assert_eq!(
            allocation_directory(root, hostile),
            Err(AgentError::EscapingPath),
            "{hostile:?} must be refused, not cleaned up"
        );
    }
    assert_eq!(
        allocation_directory(root, &"x".repeat(129)),
        Err(AgentError::EscapingPath)
    );
}
