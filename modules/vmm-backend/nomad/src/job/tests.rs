//! The mapping is what `contracts/vmm/nomad/mapping.json` says it is. These tests
//! read the contract and fail when the code drifts from it.

use aseman_domain::vmm::{
    Artifact, ArtifactKind, Bootstrap, DesiredStatus, IngressPort, NetworkPolicy, Resources,
    WorkloadLabels, WorkloadSpec,
};
use aseman_domain::{Generation, Uuid};

use super::*;

const CONTRACT: &str = include_str!("../../../../../contracts/vmm/nomad/mapping.json");

fn contract() -> Value {
    serde_json::from_str(CONTRACT).expect("the mapping contract is JSON")
}

/// A network that really denies egress, which is what a workload's default policy
/// asks for.
fn restricted() -> NetworkMode {
    NetworkMode::Cni {
        name: "aseman-restricted".to_owned(),
        denies_egress: true,
    }
}

fn record(state: DesiredWorkloadState) -> WorkloadRecord {
    WorkloadRecord {
        owner: "node:test".to_owned(),
        id: WorkloadId::from_uuid(Uuid::from_u128(0x1234_5678_9abc_def0)),
        labels: WorkloadLabels {
            creature_id: Uuid::from_u128(1),
            program_id: Uuid::from_u128(2),
            entity_id: "web".to_owned(),
            legacy_machine_id: None,
            legacy_vm_id: None,
        },
        spec: WorkloadSpec {
            runtime: "docker".to_owned(),
            artifact: Artifact {
                kind: ArtifactKind::Oci,
                reference: "busybox:1.36".to_owned(),
                digest: "sha256:00".to_owned(),
            },
            entry: "main".to_owned(),
            resources: Resources {
                vcpu_millis: 250,
                memory_mib: 64,
                disk_mib: None,
                invocation_timeout_millis: None,
            },
            network: NetworkPolicy {
                ingress: vec![IngressPort {
                    name: "http".to_owned(),
                    port: 8080,
                    protocol: PortProtocol::Http,
                }],
                egress_allow: Vec::new(),
            },
            environment: Default::default(),
            bootstrap: Bootstrap {
                guest_api_url: "https://node.invalid/guest/v1".to_owned(),
                credential: None,
            },
        },
        desired: DesiredStatus {
            state,
            generation: Generation::INITIAL,
        },
        applied_generation: None,
        observed: None,
        resource_version: 1,
        created_at_millis: 0,
        updated_at_millis: 0,
    }
}

#[test]
fn a_job_id_names_its_workload_and_nothing_else() {
    let record = record(DesiredWorkloadState::Running);
    let id = job_id(record.id);
    assert_eq!(
        contract()["job"]["id"],
        "aseman-{workload}",
        "the contract still spells the job ID this way"
    );
    assert_eq!(id, format!("aseman-{}", record.id));
    assert_eq!(workload_of(&id), Some(record.id));
    assert_eq!(workload_of("aseman-not-a-uuid"), None);
    assert_eq!(workload_of("someone-elses-job"), None);
}

#[test]
fn the_count_follows_the_desired_state() {
    let contract = contract();
    let counts = &contract["job"]["count_by_desired_state"];
    for (state, desired) in [
        (DesiredWorkloadState::Running, "running"),
        (DesiredWorkloadState::Stopped, "stopped"),
        (DesiredWorkloadState::Deleted, "deleted"),
    ] {
        let job = job(
            &record(state),
            &Execution::Image,
            "aseman",
            &["dc1".into()],
            &restricted(),
        )
        .expect("the mapping");
        assert_eq!(
            job["TaskGroups"][0]["Count"], counts[desired],
            "the count for {desired}"
        );
    }
}

#[test]
fn a_pause_is_refused_rather_than_approximated() {
    let error = job(
        &record(DesiredWorkloadState::Paused),
        &Execution::Image,
        "aseman",
        &["dc1".into()],
        &restricted(),
    )
    .expect_err("pause");
    assert!(matches!(error, PortError::Unsupported(_)), "{error:?}");
}

#[test]
fn the_job_carries_the_generation_it_was_written_for() {
    let mut record = record(DesiredWorkloadState::Running);
    record.desired.generation = Generation::from_stored(7).expect("generation");
    let job = job(
        &record,
        &Execution::Image,
        "aseman",
        &["dc1".into()],
        &restricted(),
    )
    .expect("the mapping");
    assert_eq!(job["Meta"]["aseman.generation"], "7");
    assert_eq!(job["Meta"]["aseman.workload"], record.id.to_string());
    assert_eq!(job["Meta"]["aseman.owner"], record.owner);
    assert!(
        contract()["job"]["meta_is_observed_generation"]
            .as_bool()
            .unwrap_or(false),
        "the observation reads the generation back from meta"
    );
}

#[test]
fn a_workload_is_never_given_the_host() {
    let job = job(
        &record(DesiredWorkloadState::Running),
        &Execution::Image,
        "aseman",
        &["dc1".into()],
        &restricted(),
    )
    .expect("the mapping");
    assert_eq!(
        job["TaskGroups"][0]["Networks"][0]["Mode"],
        "cni/aseman-restricted"
    );
    assert_eq!(job["TaskGroups"][0]["Tasks"][0]["Driver"], "docker");
    let config = &job["TaskGroups"][0]["Tasks"][0]["Config"];
    for forbidden in [
        "privileged",
        "network_mode",
        "pid_mode",
        "volumes",
        "mounts",
    ] {
        assert!(
            config.get(forbidden).is_none(),
            "the mapping never sets {forbidden}"
        );
    }
    assert_ne!(
        job["TaskGroups"][0]["Tasks"][0]["Driver"], "raw_exec",
        "raw_exec is forbidden"
    );
}

#[test]
fn the_credential_is_never_written_into_the_job() {
    let mut record = record(DesiredWorkloadState::Running);
    record.spec.bootstrap.credential = Some(aseman_domain::vmm::WriteOnlyCredential::new(
        "a-secret-credential".to_owned(),
    ));
    let job = job(
        &record,
        &Execution::Image,
        "aseman",
        &["dc1".into()],
        &restricted(),
    )
    .expect("the mapping");
    let text = serde_json::to_string(&job).expect("the job is JSON");
    assert!(
        !text.contains("a-secret-credential"),
        "Nomad stores the job spec in plain text; the credential never goes in it"
    );
}

#[test]
fn ports_and_services_come_from_the_declared_ingress() {
    let job = job(
        &record(DesiredWorkloadState::Running),
        &Execution::Image,
        "aseman",
        &["dc1".into()],
        &restricted(),
    )
    .expect("the mapping");
    let ports = job["TaskGroups"][0]["Networks"][0]["DynamicPorts"]
        .as_array()
        .expect("ports");
    assert_eq!(ports.len(), 1);
    assert_eq!(ports[0]["Label"], "http");
    assert_eq!(ports[0]["To"], 8080);
    let services = job["TaskGroups"][0]["Services"]
        .as_array()
        .expect("services");
    assert_eq!(services.len(), 1, "one http port is one service");
    assert_eq!(services[0]["PortLabel"], "http");
}

#[test]
fn resources_respect_nomads_floors() {
    let mut record = record(DesiredWorkloadState::Running);
    record.spec.resources.vcpu_millis = 0;
    record.spec.resources.memory_mib = 1;
    let job = job(
        &record,
        &Execution::Image,
        "aseman",
        &["dc1".into()],
        &restricted(),
    )
    .expect("the mapping");
    assert_eq!(job["TaskGroups"][0]["Tasks"][0]["Resources"]["CPU"], 1);
    assert_eq!(
        job["TaskGroups"][0]["Tasks"][0]["Resources"]["MemoryMB"],
        10
    );
}

#[test]
fn every_client_status_the_contract_lists_maps_to_a_state() {
    let contract = contract();
    let statuses = contract["observation"]["from_client_status"]
        .as_object()
        .expect("the status map");
    for (status, expected) in statuses {
        let state = observed_state(status);
        let text = serde_json::to_value(state).expect("the state is JSON");
        assert_eq!(&text, expected, "{status}");
    }
    assert_eq!(
        observed_state("something-nomad-invented-later"),
        ObservedWorkloadState::Unknown,
        "an unknown status is unknown, not running"
    );
}

#[test]
fn a_runner_runtime_runs_the_operators_image_not_the_artifact() {
    let record = record(DesiredWorkloadState::Running);
    let job = job(
        &record,
        &Execution::Runner("aseman/runner-javascript:1".to_owned()),
        "aseman",
        &["dc1".into()],
        &restricted(),
    )
    .expect("the mapping");
    let task = &job["TaskGroups"][0]["Tasks"][0];
    assert_eq!(task["Config"]["image"], "aseman/runner-javascript:1");
    assert_eq!(task["Env"]["ASEMAN_WORKLOAD"], record.id.to_string());
    assert_eq!(task["Env"]["ASEMAN_ARTIFACT_DIGEST"], "sha256:00");
    assert_eq!(
        task["Env"]["ASEMAN_GUEST_API"],
        "https://node.invalid/guest/v1"
    );
}

#[test]
fn a_workload_that_asks_for_no_egress_is_refused_on_an_unrestricted_network() {
    // Nomad's plain bridge hands a workload the internet. A workload whose policy
    // denies egress must be refused there, not quietly placed (A406).
    let error = job(
        &record(DesiredWorkloadState::Running),
        &Execution::Image,
        "aseman",
        &["dc1".into()],
        &NetworkMode::Bridge,
    )
    .expect_err("an unrestricted network");
    assert!(matches!(error, PortError::Unsupported(_)), "{error:?}");
    assert!(
        contract()["network"]["egress_deny_by_default"]
            .as_str()
            .unwrap_or_default()
            .contains("refused"),
        "the contract says so too"
    );
}

#[test]
fn a_selective_egress_allowance_is_refused_rather_than_guessed() {
    let mut record = record(DesiredWorkloadState::Running);
    record.spec.network.egress_allow = vec!["api.example.com".to_owned()];
    let error = job(
        &record,
        &Execution::Image,
        "aseman",
        &["dc1".into()],
        &restricted(),
    )
    .expect_err("a selective allowance");
    assert!(matches!(error, PortError::Unsupported(_)), "{error:?}");
}
