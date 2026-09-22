use std::collections::BTreeSet;

use aseman_domain::vmm::{
    CommandFreshness, ReconcileAction, command_freshness, desired_transitions,
    operation_transition, reconcile,
};
use serde_json::{Value, json};

use aseman_domain::{DesiredWorkloadState, ObservedWorkloadState};

use super::*;

fn openapi() -> Value {
    serde_json::from_str(OPENAPI_JSON).unwrap()
}

fn states() -> Value {
    serde_json::from_str(STATES_JSON).unwrap()
}

fn wire<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

fn enum_values(document: &Value, schema: &str) -> Vec<String> {
    document["components"]["schemas"][schema]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect()
}

fn generation(value: u64) -> Generation {
    Generation::from_stored(value).unwrap()
}

#[test]
fn every_planned_endpoint_exists() {
    let document = openapi();
    assert_eq!(document["openapi"], "3.1.0");
    assert_eq!(document["info"]["version"], API_VERSION);
    for (method, path) in [
        ("get", "/v1/capabilities"),
        ("post", "/v1/workloads"),
        ("get", "/v1/workloads"),
        ("get", "/v1/workloads/{workload_id}"),
        ("post", "/v1/workloads/{workload_id}/start"),
        ("post", "/v1/workloads/{workload_id}/stop"),
        ("post", "/v1/workloads/{workload_id}/pause"),
        ("post", "/v1/workloads/{workload_id}/resume"),
        ("delete", "/v1/workloads/{workload_id}"),
        ("post", "/v1/workloads/{workload_id}/exec"),
        ("get", "/v1/workloads/{workload_id}/logs"),
        ("get", "/v1/workloads/{workload_id}/events"),
        ("get", "/v1/workloads/{workload_id}/usage"),
        ("get", "/v1/operations/{operation_id}"),
        ("get", "/health/live"),
        ("get", "/health/ready"),
        ("get", "/version"),
        // Beyond the plan's list, from the A006 runtime operations (A505).
        ("post", "/v1/workloads/{workload_id}/invocations"),
        ("post", "/v1/workloads/{workload_id}/http"),
        ("put", "/v1/workloads/{workload_id}/files/{path}"),
        ("get", "/v1/workloads/{workload_id}/files/{path}"),
        ("get", "/v1/workloads/{workload_id}/terminal"),
        ("post", "/v1/builds"),
        ("post", "/v1/runtimes/{runtime}/verifications"),
    ] {
        assert!(
            document["paths"][path][method].is_object(),
            "{method} {path} is missing"
        );
    }
}

#[test]
fn every_operation_follows_the_common_rules() {
    let document = openapi();
    let mut ids = BTreeSet::new();
    for (path, item) in document["paths"].as_object().unwrap() {
        for (method, operation) in item.as_object().unwrap() {
            if method == "parameters" {
                continue;
            }
            let id = operation["operationId"].as_str().unwrap();
            assert!(ids.insert(id.to_owned()), "duplicate operationId {id}");
            let references: Vec<&str> = operation["parameters"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|parameter| parameter["$ref"].as_str())
                .collect();
            let mutation = matches!(method.as_str(), "post" | "put" | "delete");
            assert_eq!(
                references.contains(&"#/components/parameters/IdempotencyKey"),
                mutation,
                "{method} {path}: idempotency key"
            );
            for common in ["RequestId", "Traceparent", "Deadline"] {
                assert!(
                    references.contains(&format!("#/components/parameters/{common}").as_str()),
                    "{method} {path}: {common}"
                );
            }
            assert_eq!(
                operation["responses"]["default"]["$ref"], "#/components/responses/Problem",
                "{method} {path}: problems"
            );
            let lists = id.starts_with("list") && id != "listWorkloadEndpoints";
            assert_eq!(
                references.contains(&"#/components/parameters/Cursor"),
                lists,
                "{method} {path}: cursor pagination"
            );
            let unauthenticated = operation["security"] == json!([]);
            assert_eq!(
                unauthenticated,
                path.starts_with("/health/"),
                "{path}: mTLS"
            );
        }
    }
}

#[test]
fn wire_enums_match_the_domain() {
    let document = openapi();
    let codes: Vec<String> = ProblemCode::ALL
        .into_iter()
        .map(|code| code.as_str().to_owned())
        .collect();
    assert_eq!(enum_values(&document, "ProblemCode"), codes);
    let table = states();
    let statuses = table["problems"].as_object().unwrap();
    assert_eq!(statuses.len(), ProblemCode::ALL.len());
    for code in ProblemCode::ALL {
        assert_eq!(
            statuses[code.as_str()],
            u64::from(code.status()),
            "{code:?}"
        );
    }
    use DesiredWorkloadState as D;
    use ObservedWorkloadState as O;
    use OperationState as S;
    let desired: Vec<String> = [D::Stopped, D::Running, D::Paused, D::Deleted]
        .into_iter()
        .map(wire)
        .collect();
    assert_eq!(enum_values(&document, "DesiredState"), desired);
    let observed: Vec<String> = [
        O::Unknown,
        O::Pending,
        O::Running,
        O::Paused,
        O::Stopped,
        O::Failed,
        O::Lost,
    ]
    .into_iter()
    .map(wire)
    .collect();
    assert_eq!(enum_values(&document, "ObservedState"), observed);
    assert_eq!(table["observed"]["states"], json!(observed));
    let operations: Vec<String> = [
        S::Pending,
        S::Running,
        S::Succeeded,
        S::Failed,
        S::Cancelled,
    ]
    .into_iter()
    .map(wire)
    .collect();
    assert_eq!(enum_values(&document, "OperationState"), operations);
    let kinds: Vec<String> = [
        OperationKind::Create,
        OperationKind::Start,
        OperationKind::Stop,
        OperationKind::Pause,
        OperationKind::Resume,
        OperationKind::Delete,
        OperationKind::Restart,
        OperationKind::UpdateSpec,
        OperationKind::Invoke,
        OperationKind::Exec,
        OperationKind::Build,
        OperationKind::Snapshot,
        OperationKind::Restore,
        OperationKind::CopyFiles,
    ]
    .into_iter()
    .map(wire)
    .collect();
    assert_eq!(enum_values(&document, "OperationKind"), kinds);
}

#[test]
fn the_state_tables_are_the_domain_rules() {
    let table = states();
    let desired: Vec<Value> = desired_transitions()
        .into_iter()
        .map(|(from, to)| json!([from, to]))
        .collect();
    assert_eq!(table["desired"]["transitions"], json!(desired));
    use OperationState as S;
    let all = [
        S::Pending,
        S::Running,
        S::Succeeded,
        S::Failed,
        S::Cancelled,
    ];
    let mut operation: Vec<Value> = all
        .iter()
        .flat_map(|from| all.iter().map(move |to| (*from, *to)))
        .filter(|(from, to)| operation_transition(*from, *to).is_ok())
        .map(|(from, to)| json!([from, to]))
        .collect();
    operation.sort_by_key(ToString::to_string);
    let mut listed = table["operation"]["transitions"]
        .as_array()
        .unwrap()
        .clone();
    listed.sort_by_key(ToString::to_string);
    assert_eq!(listed, operation);
    for case in table["commands"]["cases"].as_array().unwrap() {
        let applied = case["applied"].as_u64().map(generation);
        let result = command_freshness(applied, generation(case["incoming"].as_u64().unwrap()));
        let expected: CommandFreshness = serde_json::from_value(case["result"].clone()).unwrap();
        assert_eq!(result, expected, "{case}");
    }
    for case in table["reconcile"]["cases"].as_array().unwrap() {
        let desired = case["desired"].as_array().map(|pair| {
            (
                serde_json::from_value(pair[0].clone()).unwrap(),
                generation(pair[1].as_u64().unwrap()),
            )
        });
        let observed = case["observed"]
            .as_array()
            .map(|pair| aseman_domain::vmm::Observation {
                state: serde_json::from_value(pair[0].clone()).unwrap(),
                generation: generation(pair[1].as_u64().unwrap()),
                sequence: 1,
                reason: None,
                observed_at_millis: 0,
            });
        let expected: ReconcileAction = serde_json::from_value(case["action"].clone()).unwrap();
        assert_eq!(reconcile(desired, observed.as_ref()), expected, "{case}");
    }
}

fn labels() -> WorkloadLabels {
    WorkloadLabels {
        creature_id: Uuid::from_bytes([1; 16]),
        program_id: Uuid::from_bytes([2; 16]),
        entity_id: "main".to_owned(),
        legacy_machine_id: Some("5@global".to_owned()),
        legacy_vm_id: Some("vm-1".to_owned()),
    }
}

fn artifact() -> Artifact {
    Artifact {
        kind: ArtifactKind::Blob,
        reference: "programs/5/main.wasm".to_owned(),
        digest: format!("sha256:{}", "a".repeat(64)),
    }
}

fn spec() -> WorkloadSpec {
    WorkloadSpec {
        runtime: "wasm".to_owned(),
        artifact: artifact(),
        entry: "main.wasm".to_owned(),
        resources: Resources {
            vcpu_millis: 500,
            memory_mib: 128,
            disk_mib: Some(64),
            invocation_timeout_millis: Some(30_000),
        },
        network: NetworkPolicy {
            ingress: vec![IngressPort {
                name: "web".to_owned(),
                port: 8080,
                protocol: PortProtocol::Http,
            }],
            egress_allow: vec!["api.example".to_owned()],
        },
        environment: BTreeMap::from([("MODE".to_owned(), "prod".to_owned())]),
        bootstrap: Bootstrap {
            guest_api_url: "https://node.internal/guest".to_owned(),
            credential: Some(WriteOnlyCredential::new("secret-token".to_owned())),
        },
    }
}

fn observation() -> Observation {
    Observation {
        state: ObservedWorkloadState::Running,
        generation: generation(2),
        sequence: 7,
        reason: Some("started".to_owned()),
        observed_at_millis: 10,
    }
}

fn problem() -> Problem {
    Problem {
        detail: Some("detail".to_owned()),
        instance: Some("/v1/workloads/x".to_owned()),
        retry_after_seconds: Some(1),
        current_generation: Some(3),
        ..Problem::new(ProblemCode::StaleGeneration, "Stale generation", "r1")
    }
}

fn operation() -> Operation {
    Operation {
        id: Uuid::from_bytes([3; 16]),
        workload_id: Some(Uuid::from_bytes([4; 16])),
        kind: OperationKind::Exec,
        state: OperationState::Failed,
        generation: Some(generation(2)),
        created_at_millis: 1,
        updated_at_millis: 2,
        deadline_millis: Some(3),
        result: Some(OperationResult::Exec(ExecResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            truncated: false,
        })),
        error: Some(problem()),
    }
}

fn workload() -> Workload {
    Workload {
        id: Uuid::from_bytes([4; 16]),
        labels: labels(),
        spec: spec(),
        desired: DesiredStatus {
            state: DesiredWorkloadState::Running,
            generation: generation(2),
        },
        applied_generation: Some(generation(2)),
        observed: Some(observation()),
        resource_version: "v7".to_owned(),
        created_at_millis: 1,
        updated_at_millis: 2,
    }
}

fn value<T: Serialize>(item: &T) -> Value {
    serde_json::to_value(item).unwrap()
}

/// A complete value (every optional member present) for every object schema.
fn samples() -> Vec<(&'static str, Value)> {
    vec![
        ("Problem", value(&problem())),
        ("Artifact", value(&artifact())),
        ("Resources", value(&spec().resources)),
        ("IngressPort", value(&spec().network.ingress[0])),
        ("NetworkPolicy", value(&spec().network)),
        ("Bootstrap", value(&spec().bootstrap)),
        ("WorkloadSpec", value(&spec())),
        ("WorkloadLabels", value(&labels())),
        ("DesiredStatus", value(&workload().desired)),
        ("Observation", value(&observation())),
        (
            "CreateWorkload",
            value(&CreateWorkload {
                id: Uuid::from_bytes([4; 16]),
                labels: labels(),
                spec: spec(),
                desired: workload().desired,
            }),
        ),
        ("Workload", value(&workload())),
        (
            "WorkloadPage",
            value(&Page {
                items: vec![workload()],
                next_cursor: Some("c".to_owned()),
            }),
        ),
        (
            "LifecycleCommand",
            value(&LifecycleCommand {
                generation: generation(1),
            }),
        ),
        (
            "UpdateSpec",
            value(&UpdateSpec {
                generation: generation(2),
                spec: spec(),
            }),
        ),
        (
            "Invocation",
            value(&Invocation {
                kind: InvocationKind::Signal,
                key: "tick".to_owned(),
                store_id: Some("s1".to_owned()),
                payload: json!({"n": 1}),
            }),
        ),
        (
            "ExecRequest",
            value(&ExecRequest {
                command: vec!["ls".to_owned()],
                stdin: Some(String::new()),
                timeout_millis: Some(1),
            }),
        ),
        (
            "ExecResult",
            value(&ExecResult {
                exit_code: 1,
                stdout: "b2s".to_owned(),
                stderr: String::new(),
                truncated: true,
            }),
        ),
        (
            "HttpRequest",
            value(&HttpRequest {
                method: "GET".to_owned(),
                path: "/".to_owned(),
                query: Some("a=1".to_owned()),
                headers: BTreeMap::new(),
                body: Some(String::new()),
                port: Some("web".to_owned()),
            }),
        ),
        (
            "HttpResponse",
            value(&HttpResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: Some(String::new()),
            }),
        ),
        (
            "BuildRequest",
            value(&BuildRequest {
                id: Uuid::from_bytes([5; 16]),
                runtime: "docker".to_owned(),
                source: artifact(),
                entry: Some("Dockerfile".to_owned()),
                build_type: Some("dockerfile".to_owned()),
                labels: labels(),
            }),
        ),
        (
            "BuildResult",
            value(&BuildResult {
                artifact: artifact(),
            }),
        ),
        (
            "RestoreRequest",
            value(&RestoreRequest {
                generation: generation(3),
                snapshot_id: Uuid::from_bytes([6; 16]),
            }),
        ),
        (
            "SnapshotResult",
            value(&SnapshotResult {
                snapshot_id: Uuid::from_bytes([6; 16]),
                size_bytes: 9,
            }),
        ),
        (
            "InvocationResult",
            value(&InvocationResult {
                output: Some(json!("done")),
                gas_used: Some(3),
                proof: Some("cHJvb2Y".to_owned()),
            }),
        ),
        (
            "VerificationRequest",
            value(&VerificationRequest {
                program: artifact(),
                inputs: vec![1],
                outputs: vec![2],
                proof: "cHJvb2Y".to_owned(),
            }),
        ),
        (
            "VerificationResult",
            value(&VerificationResult {
                valid: false,
                security_level: Some(96),
                reason: Some("bad proof".to_owned()),
            }),
        ),
        ("Operation", value(&operation())),
        (
            "OperationPage",
            value(&Page {
                items: vec![operation()],
                next_cursor: Some("c".to_owned()),
            }),
        ),
        (
            "Endpoint",
            value(&Endpoint {
                name: "web".to_owned(),
                protocol: PortProtocol::Http,
                address: "10.0.0.2".to_owned(),
                port: 8080,
            }),
        ),
        ("EndpointList", value(&EndpointList { items: vec![] })),
        ("Usage", value(&Usage::default())),
        (
            "LogRecord",
            value(&LogRecord {
                sequence: 1,
                at_millis: 2,
                stream: LogStream::Stdout,
                line: "hi".to_owned(),
            }),
        ),
        (
            "WorkloadEvent",
            value(&WorkloadEvent {
                sequence: 1,
                workload_id: Uuid::from_bytes([4; 16]),
                at_millis: 2,
                event_type: WorkloadEventType::Observed,
                observation: Some(observation()),
                operation: Some(operation()),
            }),
        ),
        (
            "RuntimeCapabilities",
            value(&RuntimeCapabilities {
                runtime: "wasm".to_owned(),
                invocation: true,
                ..RuntimeCapabilities::default()
            }),
        ),
        (
            "DeployConventions",
            value(&aseman_domain::vmm::DeployConventions {
                entity_file_name: "module.wasm".to_owned(),
                accepts_extra_files: false,
                build_on_deploy: true,
            }),
        ),
        (
            "Capabilities",
            value(&Capabilities {
                api_version: API_VERSION.to_owned(),
                backend: BackendInfo {
                    name: "native-legacy".to_owned(),
                    version: "1".to_owned(),
                },
                runtimes: vec![],
                max_request_bytes: 1,
            }),
        ),
        (
            "Health",
            value(&Health {
                status: HealthStatus::Ok,
                checks: BTreeMap::from([("backend".to_owned(), "ok".to_owned())]),
            }),
        ),
        (
            "Version",
            value(&Version {
                api_version: API_VERSION.to_owned(),
                service: "aseman-vmm".to_owned(),
                build: "dev".to_owned(),
                backend_contract: "1".to_owned(),
            }),
        ),
    ]
}

#[test]
fn the_rust_types_are_the_openapi_schemas() {
    let document = openapi();
    let schemas = document["components"]["schemas"].as_object().unwrap();
    let samples = samples();
    let covered: BTreeSet<&str> = samples.iter().map(|(name, _)| *name).collect();
    let objects: BTreeSet<&str> = schemas
        .iter()
        .filter(|(_, schema)| schema["type"] == "object")
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(covered, objects, "every object schema has a Rust type");
    for (name, sample) in samples {
        let schema = &schemas[name];
        let properties: BTreeSet<&str> = schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let members: BTreeSet<&str> = sample
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(members, properties, "{name}: members");
        for required in schema["required"].as_array().unwrap() {
            assert!(
                members.contains(required.as_str().unwrap()),
                "{name}: {required} is required"
            );
        }
    }
}

#[test]
fn credentials_are_write_only() {
    let spec = spec();
    assert!(!format!("{spec:?}").contains("secret-token"));
    let returned = serde_json::to_value(spec.redacted()).unwrap();
    assert!(returned["bootstrap"].get("credential").is_none());
    assert!(
        serde_json::from_value::<WorkloadSpec>(json!({
            "runtime": "wasm", "artifact": artifact(), "entry": "m",
            "resources": {"vcpu_millis": 1, "memory_mib": 1},
            "network": {"ingress": [], "egress_allow": []},
            "bootstrap": {"guest_api_url": "u"}, "extra": 1
        }))
        .is_err()
    );
}

#[test]
fn lifecycle_errors_map_to_problems() {
    use aseman_domain::vmm::LifecycleError as E;
    assert_eq!(ProblemCode::from(E::StaleGeneration).status(), 409);
    assert_eq!(
        ProblemCode::from(E::Unsupported),
        ProblemCode::UnsupportedOperation
    );
    assert_eq!(ProblemCode::from(E::Deleted), ProblemCode::WorkloadDeleted);
    assert!(ProblemCode::Unavailable.retryable());
    assert!(!ProblemCode::StaleGeneration.retryable());
    assert_eq!(
        problem().type_uri,
        "https://aseman.dev/problems/vmm/stale_generation"
    );
}
