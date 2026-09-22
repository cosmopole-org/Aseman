use std::sync::Arc;

use aseman_domain::WorkloadId;
use aseman_domain::vmm::{DeployConventions, RuntimeCapabilities};
use aseman_ports::conformance::vmm::{ScriptedBackend, sample_workload};
use aseman_vmm_backend_conformance::check_backend;

use super::*;
use crate::server::BackendService;

fn scripted() -> ScriptedBackend {
    ScriptedBackend::new(vec![RuntimeCapabilities {
        runtime: "wasm".to_owned(),
        invocation: true,
        files: true,
        deploy: DeployConventions {
            entity_file_name: "module.wasm".to_owned(),
            ..DeployConventions::default()
        },
        ..RuntimeCapabilities::default()
    }])
}

const INVOCATION: &str = r#"{"kind":"signal","key":"tick","payload":{}}"#;

#[test]
fn the_reference_backend_conforms() {
    check_backend(
        &scripted(),
        sample_workload("node-a", WorkloadId::new(), "wasm"),
        INVOCATION,
    );
}

#[test]
fn a504_is_transparent_to_the_backend() {
    let server_runtime = tokio::runtime::Runtime::new().unwrap();
    let listener = server_runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let address = listener.local_addr().unwrap();
    let backend: Arc<dyn VmmBackend> = Arc::new(scripted());
    server_runtime.spawn(async move {
        tonic::transport::Server::builder()
            .add_service(BackendService::new(backend).into_server())
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    let client =
        GrpcBackend::connect(&format!("http://{address}"), Duration::from_secs(10)).unwrap();
    check_backend(
        &client,
        sample_workload("node-a", WorkloadId::new(), "wasm"),
        INVOCATION,
    );
    // Errors keep their meaning across the transport.
    let unknown = sample_workload("node-a", WorkloadId::new(), "wasm");
    assert_eq!(
        client.get_file(&unknown, "nothing"),
        Err(PortError::NotFound)
    );
    // Plaintext A504 stays on this host.
    for remote in [
        "http://10.0.0.5:9000",
        "https://127.0.0.1:9000",
        "http://example.com:1",
    ] {
        assert!(matches!(
            GrpcBackend::connect(remote, Duration::from_secs(1)),
            Err(PortError::Denied(_))
        ));
    }
    for local in ["http://localhost:1", "http://[::1]:1"] {
        assert!(
            GrpcBackend::connect(local, Duration::from_secs(1)).is_ok(),
            "{local}"
        );
    }
}
