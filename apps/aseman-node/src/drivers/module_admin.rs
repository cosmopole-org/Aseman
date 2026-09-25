//! Authenticated module-supervisor administration composed by the node executable.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aseman_contracts::module::{AmodBundle, ModuleKind, ModuleManifest, ModulePermissions};
use aseman_contracts::module_control_v1::module_control_client::ModuleControlClient;
use aseman_contracts::module_control_v1::{HealthRequest, LifecycleRequest};
use aseman_module_runtime::{
    ArtifactCache, ArtifactVerifier, ConformanceReport, ConformanceSuite, ModuleError,
    ModuleLaunch, ModuleProcess, ModuleProcessFactory, ModuleResult, ModuleSupervisor,
    SpawnedModule, TrustRoot, TrustStore, sha256_digest, validate_manifest,
};
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug)]
pub struct AdminError {
    pub status: u16,
    pub message: String,
}

impl AdminError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: 400,
            message: message.into(),
        }
    }

    fn conflict(error: ModuleError) -> Self {
        Self {
            status: 409,
            message: error.to_string(),
        }
    }
}

pub trait ModuleAdministration: Send + Sync {
    fn handle(&self, method: &str, path: &str, body: &[u8]) -> Result<Value, AdminError>;
}

pub struct ModuleAdminService {
    supervisor: Mutex<ModuleSupervisor>,
    trust_roots: Mutex<BTreeMap<String, TrustDocument>>,
    trust_path: PathBuf,
    launcher: LocalModuleLauncher,
}

impl ModuleAdminService {
    pub fn open(root: impl AsRef<Path>) -> anyhow::Result<Self> {
        let root = root.as_ref();
        fs::create_dir_all(root)?;
        let trust_path = root.join("publisher-trust.json");
        let documents: Vec<TrustDocument> = if trust_path.exists() {
            serde_json::from_slice(&fs::read(&trust_path)?)?
        } else {
            Vec::new()
        };
        let mut trust = TrustStore::default();
        let mut trust_roots = BTreeMap::new();
        for document in documents {
            trust.enroll(document.to_runtime()?)?;
            trust_roots.insert(document.key_id.clone(), document);
        }
        let cache = ArtifactCache::new(root.join("cache"))?;
        Ok(Self {
            supervisor: Mutex::new(ModuleSupervisor::new(ArtifactVerifier::new(trust), cache)),
            trust_roots: Mutex::new(trust_roots),
            trust_path,
            launcher: LocalModuleLauncher {
                root: root.join("staged"),
                sequence: Arc::new(AtomicU64::new(0)),
            },
        })
    }

    fn supervisor(&self) -> Result<std::sync::MutexGuard<'_, ModuleSupervisor>, AdminError> {
        self.supervisor.lock().map_err(|_| AdminError {
            status: 503,
            message: "module supervisor lock is unavailable".to_owned(),
        })
    }

    fn persist_trust(&self, roots: &BTreeMap<String, TrustDocument>) -> Result<(), AdminError> {
        let sequence = self.launcher.sequence.fetch_add(1, Ordering::Relaxed);
        let temporary = self
            .trust_path
            .with_extension(format!("tmp-{}-{sequence}", std::process::id()));
        let encoded = serde_json::to_vec_pretty(&roots.values().collect::<Vec<_>>())
            .map_err(|error| AdminError::bad_request(error.to_string()))?;
        fs::write(&temporary, encoded).map_err(io_error)?;
        fs::rename(&temporary, &self.trust_path).map_err(io_error)
    }
}

impl ModuleAdministration for ModuleAdminService {
    fn handle(&self, method: &str, path: &str, body: &[u8]) -> Result<Value, AdminError> {
        let suffix = path
            .strip_prefix("/v1/admin/modules")
            .ok_or_else(|| AdminError::bad_request("invalid module administration path"))?;
        if method == "GET" && suffix.is_empty() {
            let supervisor = self.supervisor()?;
            return Ok(json!({
                "modules": supervisor.list(),
                "routingGeneration": supervisor.routing().generation,
            }));
        }
        if method == "POST" && suffix == "/trust" {
            let request: TrustRequest = parse_body(body)?;
            let document: TrustDocument =
                serde_json::from_str(&request.public_key).map_err(|error| {
                    AdminError::bad_request(format!("invalid trust document: {error}"))
                })?;
            if document.revoked {
                return Err(AdminError::bad_request(
                    "a revoked publisher cannot be enrolled",
                ));
            }
            let root = document.to_runtime().map_err(AdminError::conflict)?;
            let mut roots = self.trust_roots.lock().map_err(|_| AdminError {
                status: 503,
                message: "publisher trust lock is unavailable".to_owned(),
            })?;
            let mut supervisor = self.supervisor()?;
            supervisor
                .enroll_trust_root(root)
                .map_err(AdminError::conflict)?;
            roots.insert(document.key_id.clone(), document.clone());
            self.persist_trust(&roots)?;
            return Ok(json!({"ok": true, "keyId": document.key_id}));
        }
        if method == "POST" && suffix == "/install" {
            let request: InstallRequest = parse_body(body)?;
            require_node_scope(&request.scope)?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(request.artifact_base64)
                .map_err(|error| {
                    AdminError::bad_request(format!("invalid artifact encoding: {error}"))
                })?;
            let key = self
                .supervisor()?
                .install_amod(&bytes)
                .map_err(AdminError::conflict)?;
            return Ok(json!({"ok": true, "module": key, "scope": "node"}));
        }

        let (key, operation) = parse_module_operation(suffix)?;
        if method == "GET" && operation.is_none() {
            let status = self
                .supervisor()?
                .status(key)
                .map_err(AdminError::conflict)?;
            return serde_json::to_value(status)
                .map_err(|error| AdminError::bad_request(error.to_string()));
        }
        if method != "POST" {
            return Err(AdminError {
                status: 405,
                message: "method not allowed".to_owned(),
            });
        }
        match operation {
            Some("configure") => {
                let request: ConfigureRequest = parse_body(body)?;
                self.supervisor()?
                    .configure(key, &request.configuration)
                    .map_err(AdminError::conflict)?;
                Ok(json!({"ok": true, "module": key, "state": "configured"}))
            }
            Some("validate") => {
                require_request_node_scope(body)?;
                let mut supervisor = self.supervisor()?;
                let kind = supervisor.status(key).map_err(AdminError::conflict)?.kind;
                supervisor
                    .validate(
                        key,
                        &StaticConformance {
                            expected_kind: kind,
                        },
                    )
                    .map_err(AdminError::conflict)?;
                Ok(json!({"ok": true, "module": key, "state": "validated"}))
            }
            Some("stage") => {
                require_request_node_scope(body)?;
                self.supervisor()?
                    .stage(key, &self.launcher)
                    .map_err(AdminError::conflict)?;
                Ok(json!({"ok": true, "module": key, "state": "ready"}))
            }
            Some("activate") => {
                require_request_node_scope(body)?;
                let generation = self
                    .supervisor()?
                    .activate(key)
                    .map_err(AdminError::conflict)?;
                Ok(json!({"ok": true, "module": key, "routingGeneration": generation}))
            }
            Some("drain") => {
                require_request_node_scope(body)?;
                self.supervisor()?
                    .drain(key)
                    .map_err(AdminError::conflict)?;
                Ok(json!({"ok": true, "module": key, "state": "draining"}))
            }
            Some("rollback") => {
                require_request_node_scope(body)?;
                let mut supervisor = self.supervisor()?;
                let kind = supervisor.status(key).map_err(AdminError::conflict)?.kind;
                let generation = supervisor.rollback(kind).map_err(AdminError::conflict)?;
                Ok(json!({"ok": true, "module": key, "routingGeneration": generation}))
            }
            _ => Err(AdminError {
                status: 404,
                message: "module administration route not found".to_owned(),
            }),
        }
    }
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct TrustDocument {
    key_id: String,
    algorithm: String,
    public_key_hex: String,
    fingerprint: String,
    revoked: bool,
    #[serde(default)]
    revoked_at_unix_millis: Option<i64>,
}

impl TrustDocument {
    fn to_runtime(&self) -> ModuleResult<TrustRoot> {
        if self.algorithm != "ed25519" || self.revoked {
            return Err(ModuleError::InvalidSignatureEncoding);
        }
        let public_key =
            hex::decode(&self.public_key_hex).map_err(|_| ModuleError::InvalidSignatureEncoding)?;
        if self.fingerprint != sha256_digest(&public_key) {
            return Err(ModuleError::InvalidSignatureEncoding);
        }
        Ok(TrustRoot {
            key_id: self.key_id.clone(),
            public_key,
            fingerprint: self.fingerprint.clone(),
            revoked: false,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TrustRequest {
    public_key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InstallRequest {
    artifact_base64: String,
    scope: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigureRequest {
    configuration: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeRequest {
    scope: String,
}

fn parse_body<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, AdminError> {
    serde_json::from_slice(body)
        .map_err(|error| AdminError::bad_request(format!("invalid request body: {error}")))
}

fn require_request_node_scope(body: &[u8]) -> Result<(), AdminError> {
    let request: ScopeRequest = parse_body(body)?;
    require_node_scope(&request.scope)
}

fn require_node_scope(scope: &str) -> Result<(), AdminError> {
    if scope == "node" {
        Ok(())
    } else if scope == "cluster" {
        Err(AdminError {
            status: 501,
            message: "cluster-scoped module changes require placement quorum composition"
                .to_owned(),
        })
    } else {
        Err(AdminError::bad_request("scope must be node or cluster"))
    }
}

fn parse_module_operation(suffix: &str) -> Result<(&str, Option<&str>), AdminError> {
    let parts: Vec<_> = suffix.trim_start_matches('/').split('/').collect();
    if parts.is_empty()
        || parts.len() > 2
        || parts[0].is_empty()
        || matches!(parts[0], "." | "..")
        || !parts[0]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'@'))
    {
        return Err(AdminError::bad_request("invalid module key"));
    }
    if let Some(operation) = parts.get(1) {
        if !matches!(
            *operation,
            "configure" | "validate" | "stage" | "activate" | "drain" | "rollback"
        ) {
            return Err(AdminError::bad_request("invalid module operation"));
        }
    }
    Ok((parts[0], parts.get(1).copied()))
}

fn io_error(error: std::io::Error) -> AdminError {
    AdminError {
        status: 500,
        message: error.to_string(),
    }
}

struct StaticConformance {
    expected_kind: ModuleKind,
}

impl ConformanceSuite for StaticConformance {
    fn validate(
        &self,
        manifest: &ModuleManifest,
        artifact: &Path,
    ) -> ModuleResult<ConformanceReport> {
        validate_manifest(manifest)?;
        if manifest.kind != self.expected_kind || !artifact.is_file() {
            return Err(ModuleError::Conformance(
                "manifest kind or verified cache entry is invalid".to_owned(),
            ));
        }
        Ok(ConformanceReport {
            suite_version: "module-v1".to_owned(),
            passed_cases: vec![
                "manifest.closed".to_owned(),
                "artifact.verified-cache".to_owned(),
            ],
        })
    }
}

struct LocalModuleLauncher {
    root: PathBuf,
    sequence: Arc<AtomicU64>,
}

impl ModuleProcessFactory for LocalModuleLauncher {
    fn spawn(&self, launch: ModuleLaunch) -> ModuleResult<SpawnedModule> {
        if launch.permissions != ModulePermissions::default() {
            return Err(ModuleError::PermissionEnforcement(
                "the local launcher currently supports only zero-host-permission modules"
                    .to_owned(),
            ));
        }
        let bundle: AmodBundle = serde_json::from_slice(&fs::read(&launch.artifact_path)?)
            .map_err(|error| ModuleError::Process(error.to_string()))?;
        let executable_path = launch.command[0].trim_start_matches('/');
        let executable = bundle
            .files
            .iter()
            .find(|entry| entry.path == executable_path)
            .ok_or_else(|| ModuleError::Process("module executable is absent".to_owned()))?;
        let executable = base64::engine::general_purpose::STANDARD
            .decode(&executable.content_base64)
            .map_err(|error| ModuleError::Process(error.to_string()))?;
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        let directory = self
            .root
            .join(format!("process-{}-{sequence}", std::process::id()));
        fs::create_dir_all(&directory)?;
        let path = directory.join("module");
        fs::write(&path, executable)?;
        let configuration_path = directory.join("configuration.json");
        fs::write(&configuration_path, launch.configuration)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o500))?;
            fs::set_permissions(&configuration_path, fs::Permissions::from_mode(0o400))?;
        }
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let endpoint = listener.local_addr()?;
        drop(listener);
        Ok(SpawnedModule {
            process: Box::new(LocalModuleProcess {
                executable: path,
                arguments: launch.command.into_iter().skip(1).collect(),
                configuration_path,
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

struct LocalModuleProcess {
    executable: PathBuf,
    arguments: Vec<String>,
    configuration_path: PathBuf,
    endpoint: SocketAddr,
    child: Option<Child>,
    runtime: tokio::runtime::Runtime,
}

impl LocalModuleProcess {
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

impl ModuleProcess for LocalModuleProcess {
    fn start(&mut self) -> ModuleResult<()> {
        self.child = Some(
            Command::new(&self.executable)
                .args(&self.arguments)
                .arg(self.endpoint.to_string())
                .arg(&self.configuration_path)
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
            for _ in 0..50 {
                if let Ok(mut client) = ModuleControlClient::connect(endpoint.clone()).await {
                    if client
                        .health(HealthRequest::default())
                        .await
                        .map(|response| response.into_inner().ready)
                        .unwrap_or(false)
                    {
                        return Ok(true);
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
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
        let result = self.lifecycle("stop");
        if let Some(child) = self.child.as_mut() {
            for _ in 0..20 {
                if child.try_wait()?.is_some() {
                    self.child = None;
                    return result;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            child.kill()?;
            child.wait()?;
            self.child = None;
        }
        result
    }
}

impl Drop for LocalModuleProcess {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_paths_and_cluster_scope_fail_closed() {
        assert_eq!(
            parse_module_operation("/sample@1.0.0/activate").unwrap(),
            ("sample@1.0.0", Some("activate"))
        );
        assert!(parse_module_operation("/../trust").is_err());
        assert!(require_node_scope("node").is_ok());
        assert_eq!(require_node_scope("cluster").unwrap_err().status, 501);
    }
}
