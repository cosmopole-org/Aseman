//! The agent against the real Firecracker binary (A603, ADR 0010).
//!
//! This host has no `/dev/kvm`, so no microVM boots here. That is not a reason to test
//! against a double: Firecracker's API socket, its configuration handling, and its
//! refusals are all real, and what must be proven most is that the agent reports a
//! host that cannot boot rather than pretending it did.
//!
//! Skipped when no `firecracker` binary is installed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use aseman_domain::agent::{
    AgentError, AgentOperation, Grant, MachineProfile, MachineState, allocation_directory,
};
use aseman_vmm_agent::firecracker::{Machine, kvm_available};
use aseman_vmm_agent::host::{Agent, HostConfig, Refusal};

fn firecracker() -> Option<PathBuf> {
    let path = PathBuf::from("/usr/local/bin/firecracker");
    path.exists().then_some(path).or_else(|| {
        std::process::Command::new("which")
            .arg("firecracker")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_owned()))
    })
}

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("aseman-agent-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("a scratch directory");
    path
}

fn profile() -> MachineProfile {
    MachineProfile {
        name: "small".to_owned(),
        vcpu_count: 1,
        memory_mib: 128,
        // No kernel: the administrator has not supplied one on this host, so a boot
        // must fail with Firecracker's own reason rather than appear to work.
        kernel_image: PathBuf::new(),
        root_image: PathBuf::new(),
        network_profile: None,
    }
}

fn grant(operations: &[AgentOperation]) -> Grant {
    Grant {
        allocation: "alloc-one".to_owned(),
        profile: "small".to_owned(),
        expires_at_millis: i64::MAX,
        operations: operations.iter().copied().collect::<BTreeSet<_>>(),
    }
}

fn agent(root: PathBuf, binary: PathBuf, enabled: bool) -> Agent {
    Agent::new(HostConfig {
        root,
        firecracker: binary,
        profiles: BTreeMap::from([("small".to_owned(), profile())]),
        firecracker_enabled: enabled,
    })
}

#[test]
fn firecracker_is_configured_through_its_api_and_never_lies_about_booting() {
    let Some(binary) = firecracker() else {
        eprintln!("no firecracker binary; skipped");
        return;
    };
    let root = scratch("machine");
    let directory = allocation_directory(&root, "alloc-one").expect("a plain name");

    // A real Firecracker process, with a real API socket, taking a real configuration.
    let mut machine = Machine::create(&binary, directory.clone(), &profile())
        .expect("firecracker accepts the machine configuration");
    assert!(
        directory.join("firecracker.sock").exists(),
        "the API socket is where the agent put it"
    );

    // Booting: on this host it cannot work, and the agent says why in Firecracker's
    // own words instead of reporting a running machine.
    let outcome = machine.start();
    if kvm_available() {
        // A host with KVM and no kernel still cannot boot, for a different reason.
        let error = outcome.expect_err("no kernel was configured");
        assert!(
            error.to_string().contains("kernel"),
            "the reason is Firecracker's: {error}"
        );
    } else {
        let error = outcome.expect_err("this host has no KVM");
        assert!(
            !error.to_string().is_empty(),
            "a failed boot carries a reason"
        );
    }

    machine.delete();
    assert!(
        !directory.exists(),
        "a deleted machine leaves nothing behind"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_host_without_kvm_refuses_the_work_instead_of_accepting_it() {
    let Some(binary) = firecracker() else {
        eprintln!("no firecracker binary; skipped");
        return;
    };
    let root = scratch("capability");
    let agent = agent(root.clone(), binary, true);
    let grant = grant(&[AgentOperation::Create]);

    match agent.create(&grant, "alloc-one", 0) {
        Ok(state) => {
            assert!(kvm_available(), "a machine was created without KVM");
            assert_eq!(state, MachineState::Created);
        }
        Err(Refusal::Rule(AgentError::NoKvm)) => {
            assert!(!kvm_available());
            assert!(
                !agent.holds("alloc-one"),
                "a refused create leaves no machine behind"
            );
        }
        other => panic!("unexpected: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_disabled_host_refuses_before_it_looks_at_anything_else() {
    let Some(binary) = firecracker() else {
        eprintln!("no firecracker binary; skipped");
        return;
    };
    let root = scratch("disabled");
    let agent = agent(root.clone(), binary, false);
    assert_eq!(agent.capability(), Err(AgentError::Disabled));
    let error = agent
        .create(&grant(&[AgentOperation::Create]), "alloc-one", 0)
        .expect_err("a disabled host");
    assert!(
        matches!(error, Refusal::Rule(AgentError::Disabled)),
        "{error}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_grant_that_does_not_cover_the_request_is_refused_before_the_host_is_touched() {
    let Some(binary) = firecracker() else {
        eprintln!("no firecracker binary; skipped");
        return;
    };
    let root = scratch("grants");
    let agent = agent(root.clone(), binary, true);

    // The wrong allocation.
    let error = agent
        .create(&grant(&[AgentOperation::Create]), "alloc-two", 0)
        .expect_err("another allocation's grant");
    assert!(
        matches!(error, Refusal::Rule(AgentError::WrongAllocation)),
        "{error}"
    );

    // An expired grant.
    let mut expired = grant(&[AgentOperation::Create]);
    expired.expires_at_millis = 1;
    let error = agent
        .create(&expired, "alloc-one", 2)
        .expect_err("an expired grant");
    assert!(
        matches!(error, Refusal::Rule(AgentError::Expired)),
        "{error}"
    );

    // An operation the grant does not name.
    let error = agent
        .create(&grant(&[AgentOperation::State]), "alloc-one", 0)
        .expect_err("a grant that does not allow create");
    assert!(
        matches!(error, Refusal::Rule(AgentError::NotGranted)),
        "{error}"
    );

    // An allocation name that would escape the agent's root.
    let mut escaping = grant(&[AgentOperation::Create]);
    escaping.allocation = "../../etc".to_owned();
    let error = agent
        .create(&escaping, "../../etc", 0)
        .expect_err("an escaping name");
    assert!(
        matches!(
            error,
            Refusal::Rule(AgentError::EscapingPath | AgentError::NoKvm)
        ),
        "{error}"
    );

    assert!(!agent.holds("alloc-one"));
    let _ = std::fs::remove_dir_all(&root);
}
