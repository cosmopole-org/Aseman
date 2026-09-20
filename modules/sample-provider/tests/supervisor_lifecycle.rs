use aseman_contracts::module::{
    AmodBundle, AmodFile, ModuleKind, ModuleManifest, ModulePermissions, ModulePlatform,
    SignatureEnvelope,
};
use aseman_contracts::module_control_v1::module_control_client::ModuleControlClient;
use aseman_contracts::module_control_v1::{HealthRequest, LifecycleRequest, RequestMetadata};
use aseman_contracts::module_sample_v1::EchoRequest;
use aseman_contracts::module_sample_v1::sample_client::SampleClient;
use aseman_module_conformance::ModuleConformanceKit;
use aseman_module_runtime::{
    ArtifactCache, ArtifactVerifier, ModuleError, ModuleLaunch, ModuleProcess,
    ModuleProcessFactory, ModuleResult, ModuleSupervisor, SpawnedModule, TrustRoot, TrustStore,
    amod_payload_digest, artifact_signing_message, sha256_digest,
};
use base64::Engine;
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::collections::BTreeSet;
use std::fs;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TEST_SEED: [u8; 32] = [11; 32];

fn signing_key() -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&TEST_SEED).unwrap()
}

fn trust_store() -> TrustStore {
    let key = signing_key();
    let public_key = key.public_key().as_ref().to_vec();
    let mut trust = TrustStore::default();
    trust
        .enroll(TrustRoot {
            key_id: "sample-test-publisher".to_owned(),
            fingerprint: sha256_digest(&public_key),
            public_key,
            revoked: false,
        })
        .unwrap();
    trust
}

fn file(path: &str, content: &[u8]) -> AmodFile {
    AmodFile {
        path: path.to_owned(),
        content_base64: base64::engine::general_purpose::STANDARD.encode(content),
    }
}

fn bundle(version: &str, executable: &[u8]) -> Vec<u8> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = vec![
        file("usr/bin/aseman-sample-provider", executable),
        file(
            "config.schema.json",
            &fs::read(root.join("config.schema.json")).unwrap(),
        ),
        file(
            "sbom.spdx.json",
            &fs::read(root.join("sbom.spdx.json")).unwrap(),
        ),
        file(
            "licenses.json",
            &fs::read(root.join("licenses.json")).unwrap(),
        ),
    ];
    let artifact_digest = amod_payload_digest(&files).unwrap();
    let manifest = ModuleManifest {
        schema_version: 1,
        name: "sample-echo".to_owned(),
        kind: ModuleKind::Sample,
        version: version.to_owned(),
        contract: ">=1.0, <2.0".to_owned(),
        artifact_digest: artifact_digest.clone(),
        command: vec!["/usr/bin/aseman-sample-provider".to_owned()],
        health_endpoint: "/health/ready".to_owned(),
        config_schema: "config.schema.json".to_owned(),
        capabilities: vec!["sample.echo".to_owned(), "sample.health".to_owned()],
        permissions: ModulePermissions::default(),
        platforms: vec![ModulePlatform {
            os: "linux".to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
        }],
        migrations: Vec::new(),
        sbom: "sbom.spdx.json".to_owned(),
        license_manifest: "licenses.json".to_owned(),
    };
    let manifest_toml = toml::to_string(&manifest).unwrap();
    let manifest_digest = sha256_digest(manifest_toml.as_bytes());
    let signature = signing_key().sign(&artifact_signing_message(
        &artifact_digest,
        &manifest_digest,
    ));
    serde_json::to_vec(&AmodBundle {
        format_version: 1,
        manifest_toml,
        files,
        signature: SignatureEnvelope {
            algorithm: "ed25519".to_owned(),
            key_id: "sample-test-publisher".to_owned(),
            signature_hex: hex::encode(signature.as_ref()),
        },
    })
    .unwrap()
}

struct RealFactory {
    staging: PathBuf,
    sequence: AtomicUsize,
    endpoints: Arc<Mutex<Vec<SocketAddr>>>,
}

impl ModuleProcessFactory for RealFactory {
    fn spawn(&self, launch: ModuleLaunch) -> ModuleResult<SpawnedModule> {
        if launch.permissions != ModulePermissions::default() {
            return Err(ModuleError::Process(
                "sample provider must have no host permissions".to_owned(),
            ));
        }
        let bundle: AmodBundle = serde_json::from_slice(&fs::read(&launch.artifact_path)?)
            .map_err(|error| ModuleError::Process(error.to_string()))?;
        let executable = bundle
            .files
            .iter()
            .find(|entry| entry.path == "usr/bin/aseman-sample-provider")
            .ok_or_else(|| ModuleError::Process("sample executable is absent".to_owned()))?;
        let executable = base64::engine::general_purpose::STANDARD
            .decode(&executable.content_base64)
            .map_err(|error| ModuleError::Process(error.to_string()))?;
        let index = self.sequence.fetch_add(1, Ordering::SeqCst);
        let directory = self.staging.join(format!("process-{index}"));
        fs::create_dir_all(&directory)?;
        let path = directory.join("aseman-sample-provider");
        fs::write(&path, executable)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        }
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let endpoint = listener.local_addr()?;
        drop(listener);
        self.endpoints
            .lock()
            .map_err(|_| ModuleError::Process("endpoint registry unavailable".to_owned()))?
            .push(endpoint);
        Ok(SpawnedModule {
            process: Box::new(RealProcess {
                executable: path,
                endpoint,
                child: None,
                runtime: tokio::runtime::Runtime::new()
                    .map_err(|error| ModuleError::Process(error.to_string()))?,
            }),
            enforced_permissions: launch.permissions,
            injected_secret_refs: BTreeSet::new(),
        })
    }
}

struct RealProcess {
    executable: PathBuf,
    endpoint: SocketAddr,
    child: Option<Child>,
    runtime: tokio::runtime::Runtime,
}

impl RealProcess {
    fn lifecycle(&self, operation: &str) -> ModuleResult<()> {
        let endpoint = format!("http://{}", self.endpoint);
        self.runtime.block_on(async move {
            let mut client = ModuleControlClient::connect(endpoint)
                .await
                .map_err(|error| ModuleError::Process(error.to_string()))?;
            let request = LifecycleRequest::default();
            match operation {
                "restore" => client.restore(request).await,
                "drain" => client.drain(request).await,
                "stop" => client.stop(request).await,
                _ => {
                    return Err(ModuleError::Process(
                        "unknown lifecycle operation".to_owned(),
                    ));
                }
            }
            .map_err(|error| ModuleError::Process(error.to_string()))?;
            Ok(())
        })
    }
}

impl ModuleProcess for RealProcess {
    fn start(&mut self) -> ModuleResult<()> {
        self.child = Some(
            Command::new(&self.executable)
                .arg(self.endpoint.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
        );
        Ok(())
    }

    fn ready(&mut self) -> ModuleResult<bool> {
        let endpoint = format!("http://{}", self.endpoint);
        self.runtime.block_on(async move {
            for _ in 0..40 {
                if let Ok(mut client) = ModuleControlClient::connect(endpoint.clone()).await
                    && client
                        .health(HealthRequest::default())
                        .await
                        .map(|response| response.into_inner().ready)
                        .unwrap_or(false)
                {
                    return Ok(true);
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Ok(false)
        })
    }

    fn restore(&mut self) -> ModuleResult<()> {
        self.lifecycle("restore")
    }

    fn drain(&mut self) -> ModuleResult<()> {
        self.lifecycle("drain")
    }

    fn stop(&mut self) -> ModuleResult<()> {
        let _ = self.lifecycle("stop");
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            child.wait()?;
        }
        Ok(())
    }
}

impl Drop for RealProcess {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn real_provider_installs_switches_drains_and_rolls_back_without_node_rebuild() {
    let root = std::env::temp_dir().join(format!(
        "aseman-real-module-lifecycle-{}",
        std::process::id()
    ));
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    let endpoints = Arc::new(Mutex::new(Vec::new()));
    let factory = RealFactory {
        staging: root.join("staging"),
        sequence: AtomicUsize::new(0),
        endpoints: endpoints.clone(),
    };
    let verifier = ArtifactVerifier::new(trust_store());
    let cache = ArtifactCache::new(root.join("cache")).unwrap();
    let mut supervisor = ModuleSupervisor::new(verifier, cache);
    let executable = fs::read(env!("CARGO_BIN_EXE_aseman-sample-provider")).unwrap();
    let suite = ModuleConformanceKit {
        expected_kind: ModuleKind::Sample,
        required_capabilities: BTreeSet::from([
            "sample.echo".to_owned(),
            "sample.health".to_owned(),
        ]),
        max_message_bytes: 16 * 1024,
    };

    let v1 = supervisor
        .install_amod(&bundle("1.0.0", &executable))
        .unwrap();
    supervisor.validate(&v1, &suite).unwrap();
    supervisor.stage(&v1, &factory).unwrap();
    supervisor.activate(&v1).unwrap();

    let v2 = supervisor
        .install_amod(&bundle("1.1.0", &executable))
        .unwrap();
    supervisor.validate(&v2, &suite).unwrap();
    supervisor.stage(&v2, &factory).unwrap();
    supervisor.activate(&v2).unwrap();
    supervisor.drain(&v1).unwrap();
    supervisor.rollback(ModuleKind::Sample).unwrap();

    let endpoint = endpoints.lock().unwrap()[0];
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let echoed = runtime.block_on(async move {
        let mut client = SampleClient::connect(format!("http://{endpoint}"))
            .await
            .unwrap();
        client
            .echo(EchoRequest {
                meta: Some(RequestMetadata {
                    request_id: "rollback-request".to_owned(),
                    trace_id: "rollback-trace".to_owned(),
                    deadline_unix_millis: i64::MAX,
                    cancellation_id: "rollback-cancel".to_owned(),
                    idempotency_key: "rollback-idempotency".to_owned(),
                }),
                value: "restored".to_owned(),
            })
            .await
            .unwrap()
            .into_inner()
            .value
    });
    assert_eq!(echoed, "restored");
    drop(supervisor);
    fs::remove_dir_all(root).unwrap();
}
