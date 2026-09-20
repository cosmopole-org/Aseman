//! Signed artifact verification and generation-fenced module supervision.
#![forbid(unsafe_code)]

use aseman_contracts::module::{
    AmodBundle, AmodFile, BootstrapSnapshot, ModuleHandshake, ModuleKind, ModuleLifecycleState,
    ModuleManifest, ModulePermissions, SignatureEnvelope,
};
use base64::Engine;
use ring::signature;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

const ARTIFACT_DOMAIN: &[u8] = b"ASEMAN-MODULE-ARTIFACT-V1\0";
const BOOTSTRAP_DOMAIN: &[u8] = b"ASEMAN-MODULE-BOOTSTRAP-V1\0";
const AMOD_PAYLOAD_DOMAIN: &[u8] = b"ASEMAN-AMOD-PAYLOAD-V1\0";
const MAX_AMOD_FILES: usize = 1024;
const MAX_AMOD_BYTES: usize = 256 * 1024 * 1024;
static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub type ModuleResult<T> = Result<T, ModuleError>;

#[derive(Debug, Error)]
pub enum ModuleError {
    #[error("invalid module manifest: {0}")]
    InvalidManifest(String),
    #[error("artifact digest mismatch")]
    DigestMismatch,
    #[error("unknown signing key: {0}")]
    UnknownSigner(String),
    #[error("signing key is revoked: {0}")]
    RevokedSigner(String),
    #[error("only Ed25519 module signatures are accepted")]
    UnsupportedSignatureAlgorithm,
    #[error("invalid signing key or signature encoding")]
    InvalidSignatureEncoding,
    #[error("module signature verification failed")]
    InvalidSignature,
    #[error("artifact cache collision for {0}")]
    CacheCollision(String),
    #[error("artifact cache I/O failed: {0}")]
    CacheIo(#[from] std::io::Error),
    #[error("module is not installed: {0}")]
    NotInstalled(String),
    #[error("invalid module lifecycle transition: {0}")]
    InvalidTransition(String),
    #[error("module conformance failed: {0}")]
    Conformance(String),
    #[error("module did not become ready: {0}")]
    NotReady(String),
    #[error("module process operation failed: {0}")]
    Process(String),
    #[error("module protocol negotiation failed: {0}")]
    Negotiation(String),
    #[error("module permission enforcement failed: {0}")]
    PermissionEnforcement(String),
    #[error("bootstrap snapshot has expired")]
    BootstrapExpired,
    #[error("bootstrap snapshot is not yet valid")]
    BootstrapNotYetValid,
    #[error("bootstrap snapshot encoding failed: {0}")]
    BootstrapEncoding(String),
    #[error("cluster placement quorum is not satisfied")]
    PlacementQuorum,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustRoot {
    pub key_id: String,
    pub public_key: Vec<u8>,
    pub fingerprint: String,
    pub revoked: bool,
}

#[derive(Clone, Debug, Default)]
pub struct TrustStore {
    roots: BTreeMap<String, TrustRoot>,
}

impl TrustStore {
    pub fn enroll(&mut self, root: TrustRoot) -> ModuleResult<()> {
        if root.key_id.is_empty()
            || root.public_key.len() != 32
            || root.fingerprint != sha256_digest(&root.public_key)
        {
            return Err(ModuleError::InvalidSignatureEncoding);
        }
        self.roots.insert(root.key_id.clone(), root);
        Ok(())
    }

    pub fn revoke(&mut self, key_id: &str) -> ModuleResult<()> {
        let root = self
            .roots
            .get_mut(key_id)
            .ok_or_else(|| ModuleError::UnknownSigner(key_id.to_owned()))?;
        root.revoked = true;
        Ok(())
    }

    fn verifying_key(&self, envelope: &SignatureEnvelope) -> ModuleResult<&[u8]> {
        if envelope.algorithm != "ed25519" {
            return Err(ModuleError::UnsupportedSignatureAlgorithm);
        }
        let root = self
            .roots
            .get(&envelope.key_id)
            .ok_or_else(|| ModuleError::UnknownSigner(envelope.key_id.clone()))?;
        if root.revoked {
            return Err(ModuleError::RevokedSigner(envelope.key_id.clone()));
        }
        Ok(&root.public_key)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedArtifact {
    pub digest: String,
    pub manifest_digest: String,
    pub cache_digest: String,
    pub signer_key_id: String,
    bytes: Vec<u8>,
}

impl VerifiedArtifact {
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Clone, Debug)]
pub struct ArtifactVerifier {
    trust: TrustStore,
}

impl ArtifactVerifier {
    #[must_use]
    pub fn new(trust: TrustStore) -> Self {
        Self { trust }
    }

    pub fn verify(
        &self,
        artifact: &[u8],
        manifest_source: &[u8],
        expected_digest: &str,
        envelope: &SignatureEnvelope,
    ) -> ModuleResult<VerifiedArtifact> {
        let digest = sha256_digest(artifact);
        if !constant_time_text_eq(&digest, expected_digest) {
            return Err(ModuleError::DigestMismatch);
        }
        let signature_bytes = hex::decode(&envelope.signature_hex)
            .map_err(|_| ModuleError::InvalidSignatureEncoding)?;
        let manifest_digest = sha256_digest(manifest_source);
        self.verify_digest_binding(&digest, &manifest_digest, envelope, &signature_bytes)?;
        Ok(VerifiedArtifact {
            cache_digest: digest.clone(),
            digest,
            manifest_digest,
            signer_key_id: envelope.key_id.clone(),
            bytes: artifact.to_vec(),
        })
    }

    fn verify_digest_binding(
        &self,
        artifact_digest: &str,
        manifest_digest: &str,
        envelope: &SignatureEnvelope,
        signature_bytes: &[u8],
    ) -> ModuleResult<()> {
        let public_key = self.trust.verifying_key(envelope)?;
        signature::UnparsedPublicKey::new(&signature::ED25519, public_key)
            .verify(
                &artifact_signing_message(artifact_digest, manifest_digest),
                signature_bytes,
            )
            .map_err(|_| ModuleError::InvalidSignature)
    }

    pub fn verify_amod(
        &self,
        bundle_bytes: &[u8],
    ) -> ModuleResult<(ModuleManifest, VerifiedArtifact)> {
        if bundle_bytes.len() > MAX_AMOD_BYTES * 2 {
            return Err(ModuleError::InvalidManifest(
                ".amod envelope is too large".to_owned(),
            ));
        }
        let bundle: AmodBundle = serde_json::from_slice(bundle_bytes)
            .map_err(|error| ModuleError::InvalidManifest(format!("invalid .amod: {error}")))?;
        if bundle.format_version != 1 {
            return Err(ModuleError::InvalidManifest(
                "unsupported .amod format".to_owned(),
            ));
        }
        let manifest = parse_manifest(&bundle.manifest_toml)?;
        let payload_digest = amod_payload_digest(&bundle.files)?;
        if !constant_time_text_eq(&payload_digest, &manifest.artifact_digest) {
            return Err(ModuleError::DigestMismatch);
        }
        validate_amod_contents(&manifest, &bundle.files)?;
        let manifest_digest = sha256_digest(bundle.manifest_toml.as_bytes());
        let signature_bytes = hex::decode(&bundle.signature.signature_hex)
            .map_err(|_| ModuleError::InvalidSignatureEncoding)?;
        self.verify_digest_binding(
            &payload_digest,
            &manifest_digest,
            &bundle.signature,
            &signature_bytes,
        )?;
        Ok((
            manifest,
            VerifiedArtifact {
                digest: payload_digest,
                manifest_digest,
                cache_digest: sha256_digest(bundle_bytes),
                signer_key_id: bundle.signature.key_id,
                bytes: bundle_bytes.to_vec(),
            },
        ))
    }
}

#[must_use]
pub fn sha256_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

#[must_use]
pub fn artifact_signing_message(artifact_digest: &str, manifest_digest: &str) -> Vec<u8> {
    let mut message = Vec::with_capacity(
        ARTIFACT_DOMAIN.len() + artifact_digest.len() + 1 + manifest_digest.len(),
    );
    message.extend_from_slice(ARTIFACT_DOMAIN);
    message.extend_from_slice(artifact_digest.as_bytes());
    message.push(0);
    message.extend_from_slice(manifest_digest.as_bytes());
    message
}

pub fn amod_payload_digest(files: &[AmodFile]) -> ModuleResult<String> {
    if files.is_empty() || files.len() > MAX_AMOD_FILES {
        return Err(ModuleError::InvalidManifest(
            "invalid .amod file count".to_owned(),
        ));
    }
    let mut decoded = BTreeMap::new();
    let mut total = 0_usize;
    for file in files {
        validate_relative_bundle_path(&file.path)?;
        let content = base64::engine::general_purpose::STANDARD
            .decode(&file.content_base64)
            .map_err(|_| ModuleError::InvalidManifest(format!("invalid base64: {}", file.path)))?;
        total = total
            .checked_add(content.len())
            .ok_or_else(|| ModuleError::InvalidManifest(".amod size overflow".to_owned()))?;
        if total > MAX_AMOD_BYTES || decoded.insert(file.path.as_str(), content).is_some() {
            return Err(ModuleError::InvalidManifest(
                ".amod is too large or contains duplicate paths".to_owned(),
            ));
        }
    }
    let mut hasher = Sha256::new();
    hasher.update(AMOD_PAYLOAD_DOMAIN);
    for (path, content) in decoded {
        hasher.update((path.len() as u64).to_be_bytes());
        hasher.update(path.as_bytes());
        hasher.update((content.len() as u64).to_be_bytes());
        hasher.update(content);
    }
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

fn validate_amod_contents(manifest: &ModuleManifest, files: &[AmodFile]) -> ModuleResult<()> {
    let paths: BTreeSet<&str> = files.iter().map(|file| file.path.as_str()).collect();
    let executable = manifest.command[0].trim_start_matches('/');
    for required in [
        executable,
        manifest.config_schema.as_str(),
        manifest.sbom.as_str(),
        manifest.license_manifest.as_str(),
    ] {
        validate_relative_bundle_path(required)?;
        if !paths.contains(required) {
            return Err(ModuleError::InvalidManifest(format!(
                ".amod is missing required file: {required}"
            )));
        }
    }
    Ok(())
}

fn constant_time_text_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

#[derive(Clone, Debug)]
pub struct ArtifactCache {
    root: PathBuf,
}

impl ArtifactCache {
    pub fn new(root: impl Into<PathBuf>) -> ModuleResult<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn store(&self, artifact: &VerifiedArtifact) -> ModuleResult<PathBuf> {
        let digest_hex = validated_digest_hex(&artifact.cache_digest)?;
        let target = self.root.join(format!("sha256-{digest_hex}.amod"));
        if target.exists() {
            let existing = fs::read(&target)?;
            if sha256_digest(&existing) != artifact.cache_digest {
                return Err(ModuleError::CacheCollision(artifact.cache_digest.clone()));
            }
            return Ok(target);
        }
        let temporary = unique_temporary_path(&target);
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        if let Err(error) = file
            .write_all(artifact.bytes())
            .and_then(|()| file.sync_all())
            .and_then(|()| fs::rename(&temporary, &target))
        {
            if target.exists()
                && fs::read(&target)
                    .map(|bytes| sha256_digest(&bytes) == artifact.cache_digest)
                    .unwrap_or(false)
            {
                let _ = fs::remove_file(&temporary);
                return Ok(target);
            }
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
        Ok(target)
    }

    pub fn read(&self, digest: &str) -> ModuleResult<Vec<u8>> {
        let digest_hex = validated_digest_hex(digest)?;
        let bytes = fs::read(self.root.join(format!("sha256-{digest_hex}.amod")))?;
        if sha256_digest(&bytes) != digest {
            return Err(ModuleError::CacheCollision(digest.to_owned()));
        }
        Ok(bytes)
    }
}

fn unique_temporary_path(target: &Path) -> PathBuf {
    let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    target.with_extension(format!("tmp-{}-{sequence}", std::process::id()))
}

fn validated_digest_hex(digest: &str) -> ModuleResult<&str> {
    let value = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| ModuleError::InvalidManifest("digest must use sha256".to_owned()))?;
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ModuleError::InvalidManifest(
            "digest must contain 64 hexadecimal characters".to_owned(),
        ));
    }
    Ok(value)
}

pub fn parse_manifest(source: &str) -> ModuleResult<ModuleManifest> {
    let manifest: ModuleManifest =
        toml::from_str(source).map_err(|error| ModuleError::InvalidManifest(error.to_string()))?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

pub fn validate_manifest(manifest: &ModuleManifest) -> ModuleResult<()> {
    if manifest.schema_version != 1 {
        return Err(ModuleError::InvalidManifest(
            "unsupported schema_version".to_owned(),
        ));
    }
    validate_token("name", &manifest.name)?;
    Version::parse(&manifest.version)
        .map_err(|error| ModuleError::InvalidManifest(format!("version: {error}")))?;
    VersionReq::parse(&manifest.contract)
        .map_err(|error| ModuleError::InvalidManifest(format!("contract: {error}")))?;
    validated_digest_hex(&manifest.artifact_digest)?;
    let Some(executable) = manifest.command.first() else {
        return Err(ModuleError::InvalidManifest("command is empty".to_owned()));
    };
    if !executable.starts_with('/') || manifest.command.iter().any(|part| part.is_empty()) {
        return Err(ModuleError::InvalidManifest(
            "command executable must be absolute and arguments non-empty".to_owned(),
        ));
    }
    if !manifest.health_endpoint.starts_with('/') || manifest.health_endpoint.contains("..") {
        return Err(ModuleError::InvalidManifest(
            "health_endpoint must be an absolute safe path".to_owned(),
        ));
    }
    for path in [
        manifest.config_schema.as_str(),
        manifest.sbom.as_str(),
        manifest.license_manifest.as_str(),
    ] {
        validate_relative_bundle_path(path)?;
    }
    validate_unique_tokens("capability", &manifest.capabilities)?;
    validate_permissions(&manifest.permissions)?;
    if manifest.platforms.is_empty() {
        return Err(ModuleError::InvalidManifest(
            "at least one platform is required".to_owned(),
        ));
    }
    for platform in &manifest.platforms {
        validate_token("platform os", &platform.os)?;
        validate_token("platform architecture", &platform.architecture)?;
    }
    Ok(())
}

fn validate_token(field: &str, value: &str) -> ModuleResult<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(ModuleError::InvalidManifest(format!("invalid {field}")));
    }
    Ok(())
}

fn validate_unique_tokens(field: &str, values: &[String]) -> ModuleResult<()> {
    let mut seen = BTreeSet::new();
    for value in values {
        validate_token(field, value)?;
        if !seen.insert(value) {
            return Err(ModuleError::InvalidManifest(format!(
                "duplicate {field}: {value}"
            )));
        }
    }
    Ok(())
}

fn validate_relative_bundle_path(value: &str) -> ModuleResult<()> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(ModuleError::InvalidManifest(format!(
            "unsafe bundle path: {value}"
        )));
    }
    Ok(())
}

fn validate_permissions(permissions: &ModulePermissions) -> ModuleResult<()> {
    validate_unique_tokens("secret reference", &permissions.secret_refs)?;
    for mount in permissions
        .read_only_mounts
        .iter()
        .chain(&permissions.read_write_mounts)
    {
        if !Path::new(mount).is_absolute() || mount.contains("..") {
            return Err(ModuleError::InvalidManifest(format!(
                "unsafe mount path: {mount}"
            )));
        }
    }
    let read_only: BTreeSet<&str> = permissions
        .read_only_mounts
        .iter()
        .map(String::as_str)
        .collect();
    if permissions
        .read_write_mounts
        .iter()
        .any(|mount| read_only.contains(mount.as_str()))
    {
        return Err(ModuleError::InvalidManifest(
            "a mount cannot be both read-only and read-write".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NegotiatedContract {
    pub protocol_major: u16,
    pub protocol_minor: u16,
    pub capabilities: BTreeSet<String>,
    pub max_message_bytes: u64,
}

pub fn negotiate(
    client: &ModuleHandshake,
    provider: &ModuleHandshake,
    required_capabilities: &BTreeSet<String>,
) -> ModuleResult<NegotiatedContract> {
    if client.protocol.major != provider.protocol.major {
        return Err(ModuleError::Negotiation(
            "protocol major versions differ".to_owned(),
        ));
    }
    if client.provider_kind != provider.provider_kind {
        return Err(ModuleError::Negotiation("provider kinds differ".to_owned()));
    }
    if client.instance_id.is_empty() || provider.instance_id.is_empty() {
        return Err(ModuleError::Negotiation(
            "instance identity is required".to_owned(),
        ));
    }
    let client_caps: BTreeSet<String> = client.capabilities.iter().cloned().collect();
    let provider_caps: BTreeSet<String> = provider.capabilities.iter().cloned().collect();
    let capabilities: BTreeSet<String> =
        client_caps.intersection(&provider_caps).cloned().collect();
    let missing: Vec<&String> = required_capabilities.difference(&capabilities).collect();
    if !missing.is_empty() {
        return Err(ModuleError::Negotiation(format!(
            "required capabilities are unavailable: {missing:?}"
        )));
    }
    let max_message_bytes = client.max_message_bytes.min(provider.max_message_bytes);
    if max_message_bytes == 0 {
        return Err(ModuleError::Negotiation(
            "message size limit is zero".to_owned(),
        ));
    }
    Ok(NegotiatedContract {
        protocol_major: client.protocol.major,
        protocol_minor: client.protocol.minor.min(provider.protocol.minor),
        capabilities,
        max_message_bytes,
    })
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConformanceReport {
    pub suite_version: String,
    pub passed_cases: Vec<String>,
}

pub trait ConformanceSuite {
    fn validate(
        &self,
        manifest: &ModuleManifest,
        artifact: &Path,
    ) -> ModuleResult<ConformanceReport>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleLaunch {
    pub artifact_path: PathBuf,
    pub command: Vec<String>,
    pub permissions: ModulePermissions,
    /// Validated JSON configuration supplied to the launcher as a protected file.
    pub configuration: String,
}

/// Result returned by the trusted process launcher after constructing the
/// sandbox and resolving secret references. Secret values never cross this boundary.
pub struct SpawnedModule {
    pub process: Box<dyn ModuleProcess>,
    pub enforced_permissions: ModulePermissions,
    pub injected_secret_refs: BTreeSet<String>,
}

pub trait ModuleProcess: Send {
    fn start(&mut self) -> ModuleResult<()>;
    fn ready(&mut self) -> ModuleResult<bool>;
    fn restore(&mut self) -> ModuleResult<()>;
    fn drain(&mut self) -> ModuleResult<()>;
    fn stop(&mut self) -> ModuleResult<()>;
}

pub trait ModuleProcessFactory {
    /// Launchers are part of the trusted computing base. They deny unsupported
    /// permissions and report only controls the operating system actually applied.
    fn spawn(&self, launch: ModuleLaunch) -> ModuleResult<SpawnedModule>;
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleStatus {
    pub key: String,
    pub name: String,
    pub kind: ModuleKind,
    pub version: String,
    pub artifact_digest: String,
    pub state: ModuleLifecycleState,
    pub conformance: Option<ConformanceReport>,
}

struct ModuleRecord {
    manifest: ModuleManifest,
    artifact_path: PathBuf,
    state: ModuleLifecycleState,
    conformance: Option<ConformanceReport>,
    configuration: String,
    process: Option<Box<dyn ModuleProcess>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoutingGeneration {
    pub generation: u64,
    pub active: BTreeMap<ModuleKindKey, String>,
    pub previous: BTreeMap<ModuleKindKey, String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ModuleKindKey(String);

impl From<ModuleKind> for ModuleKindKey {
    fn from(value: ModuleKind) -> Self {
        let value = match value {
            ModuleKind::Storage => "storage",
            ModuleKind::ClientNetwork => "client_network",
            ModuleKind::Federation => "federation",
            ModuleKind::Security => "security",
            ModuleKind::Realtime => "realtime",
            ModuleKind::FinanceLedger => "finance_ledger",
            ModuleKind::Consensus => "consensus",
            ModuleKind::Coordination => "coordination",
            ModuleKind::Vmm => "vmm",
            ModuleKind::VmmBackend => "vmm_backend",
            ModuleKind::Runtime => "runtime",
            ModuleKind::Telemetry => "telemetry",
            ModuleKind::Sample => "sample",
        };
        Self(value.to_owned())
    }
}

pub struct ModuleSupervisor {
    verifier: ArtifactVerifier,
    cache: ArtifactCache,
    modules: BTreeMap<String, ModuleRecord>,
    routing: RoutingGeneration,
}

impl ModuleSupervisor {
    #[must_use]
    pub fn new(verifier: ArtifactVerifier, cache: ArtifactCache) -> Self {
        Self {
            verifier,
            cache,
            modules: BTreeMap::new(),
            routing: RoutingGeneration {
                generation: 0,
                active: BTreeMap::new(),
                previous: BTreeMap::new(),
            },
        }
    }

    pub fn enroll_trust_root(&mut self, root: TrustRoot) -> ModuleResult<()> {
        self.verifier.trust.enroll(root)
    }

    pub fn install(
        &mut self,
        manifest_source: &str,
        artifact_bytes: &[u8],
        signature: &SignatureEnvelope,
    ) -> ModuleResult<String> {
        let manifest = parse_manifest(manifest_source)?;
        let verified = self.verifier.verify(
            artifact_bytes,
            manifest_source.as_bytes(),
            &manifest.artifact_digest,
            signature,
        )?;
        self.register_verified(manifest, verified)
    }

    pub fn install_amod(&mut self, bundle_bytes: &[u8]) -> ModuleResult<String> {
        let (manifest, verified) = self.verifier.verify_amod(bundle_bytes)?;
        self.register_verified(manifest, verified)
    }

    fn register_verified(
        &mut self,
        manifest: ModuleManifest,
        verified: VerifiedArtifact,
    ) -> ModuleResult<String> {
        let artifact_path = self.cache.store(&verified)?;
        let key = module_key(&manifest);
        if let Some(existing) = self.modules.get(&key) {
            if existing.manifest != manifest {
                return Err(ModuleError::InvalidTransition(format!(
                    "module key {key} is already installed with different metadata"
                )));
            }
            return Ok(key);
        }
        self.modules.insert(
            key.clone(),
            ModuleRecord {
                manifest,
                artifact_path,
                state: ModuleLifecycleState::Installed,
                conformance: None,
                configuration: "{}".to_owned(),
                process: None,
            },
        );
        Ok(key)
    }

    pub fn validate(&mut self, key: &str, suite: &dyn ConformanceSuite) -> ModuleResult<()> {
        let record = self.record_mut(key)?;
        if !matches!(
            record.state,
            ModuleLifecycleState::Installed | ModuleLifecycleState::Validated
        ) {
            return Err(ModuleError::InvalidTransition(format!(
                "validate from {:?}",
                record.state
            )));
        }
        record.conformance = Some(suite.validate(&record.manifest, &record.artifact_path)?);
        record.state = ModuleLifecycleState::Validated;
        Ok(())
    }

    pub fn configure(&mut self, key: &str, configuration: &str) -> ModuleResult<()> {
        let value: serde_json::Value = serde_json::from_str(configuration)
            .map_err(|error| ModuleError::InvalidManifest(format!("configuration: {error}")))?;
        if !value.is_object() {
            return Err(ModuleError::InvalidManifest(
                "module configuration must be a JSON object".to_owned(),
            ));
        }
        let record = self.record_mut(key)?;
        if matches!(
            record.state,
            ModuleLifecycleState::Staged
                | ModuleLifecycleState::Ready
                | ModuleLifecycleState::Active
                | ModuleLifecycleState::Draining
        ) {
            return Err(ModuleError::InvalidTransition(format!(
                "configure from {:?}",
                record.state
            )));
        }
        record.configuration = serde_json::to_string(&value)
            .map_err(|error| ModuleError::InvalidManifest(error.to_string()))?;
        Ok(())
    }

    pub fn stage(&mut self, key: &str, factory: &dyn ModuleProcessFactory) -> ModuleResult<()> {
        let record = self.record_mut(key)?;
        if !matches!(
            record.state,
            ModuleLifecycleState::Validated | ModuleLifecycleState::Stopped
        ) || record.conformance.is_none()
        {
            return Err(ModuleError::InvalidTransition(format!(
                "stage from {:?}",
                record.state
            )));
        }
        let launch = ModuleLaunch {
            artifact_path: record.artifact_path.clone(),
            command: record.manifest.command.clone(),
            permissions: record.manifest.permissions.clone(),
            configuration: record.configuration.clone(),
        };
        let expected_permissions = launch.permissions.clone();
        let expected_secret_refs = expected_permissions.secret_refs.iter().cloned().collect();
        let spawned = factory.spawn(launch)?;
        if spawned.enforced_permissions != expected_permissions {
            return Err(ModuleError::PermissionEnforcement(
                "launcher receipt does not match the signed permission declaration".to_owned(),
            ));
        }
        if spawned.injected_secret_refs != expected_secret_refs {
            return Err(ModuleError::PermissionEnforcement(
                "launcher did not resolve exactly the declared secret references".to_owned(),
            ));
        }
        let mut process = spawned.process;
        record.state = ModuleLifecycleState::Staged;
        if let Err(error) = process.start() {
            record.state = ModuleLifecycleState::Failed;
            return Err(error);
        }
        match process.ready() {
            Ok(true) => {}
            Ok(false) => {
                let _ = process.stop();
                record.state = ModuleLifecycleState::Failed;
                return Err(ModuleError::NotReady(key.to_owned()));
            }
            Err(error) => {
                let _ = process.stop();
                record.state = ModuleLifecycleState::Failed;
                return Err(error);
            }
        }
        record.process = Some(process);
        record.state = ModuleLifecycleState::Ready;
        Ok(())
    }

    pub fn activate(&mut self, key: &str) -> ModuleResult<u64> {
        let (kind, state) = {
            let record = self.record(key)?;
            (ModuleKindKey::from(record.manifest.kind), record.state)
        };
        if state != ModuleLifecycleState::Ready {
            return Err(ModuleError::InvalidTransition(format!(
                "activate from {state:?}"
            )));
        }
        let old = self.routing.active.insert(kind.clone(), key.to_owned());
        match old {
            Some(old_key) if old_key != key => {
                self.routing.previous.insert(kind.clone(), old_key.clone());
                self.record_mut(&old_key)?.state = ModuleLifecycleState::Draining;
            }
            _ => {
                self.routing.previous.remove(&kind);
            }
        }
        self.record_mut(key)?.state = ModuleLifecycleState::Active;
        self.routing.generation =
            self.routing.generation.checked_add(1).ok_or_else(|| {
                ModuleError::InvalidTransition("routing generation overflow".into())
            })?;
        Ok(self.routing.generation)
    }

    pub fn drain(&mut self, key: &str) -> ModuleResult<()> {
        let record = self.record_mut(key)?;
        if !matches!(
            record.state,
            ModuleLifecycleState::Active | ModuleLifecycleState::Draining
        ) {
            return Err(ModuleError::InvalidTransition(format!(
                "drain from {:?}",
                record.state
            )));
        }
        record
            .process
            .as_mut()
            .ok_or_else(|| ModuleError::Process("missing staged process".to_owned()))?
            .drain()?;
        record.state = ModuleLifecycleState::Draining;
        Ok(())
    }

    pub fn stop(&mut self, key: &str) -> ModuleResult<()> {
        let record = self.record_mut(key)?;
        if record.state == ModuleLifecycleState::Active {
            return Err(ModuleError::InvalidTransition(
                "active module must be switched before stop".to_owned(),
            ));
        }
        if let Some(mut process) = record.process.take() {
            process.stop()?;
        }
        record.state = ModuleLifecycleState::Stopped;
        Ok(())
    }

    pub fn rollback(&mut self, kind: ModuleKind) -> ModuleResult<u64> {
        let kind = ModuleKindKey::from(kind);
        let previous =
            self.routing.previous.get(&kind).cloned().ok_or_else(|| {
                ModuleError::InvalidTransition("no rollback generation".to_owned())
            })?;
        let current = self
            .routing
            .active
            .get(&kind)
            .cloned()
            .ok_or_else(|| ModuleError::InvalidTransition("no active generation".to_owned()))?;
        self.record_mut(&previous)?
            .process
            .as_mut()
            .ok_or_else(|| {
                ModuleError::InvalidTransition("rollback process is no longer available".to_owned())
            })?
            .restore()?;
        self.routing.active.insert(kind.clone(), previous.clone());
        self.record_mut(&current)?.state = ModuleLifecycleState::Draining;
        self.record_mut(&previous)?.state = ModuleLifecycleState::Active;
        self.routing.previous.insert(kind, current);
        self.routing.generation =
            self.routing.generation.checked_add(1).ok_or_else(|| {
                ModuleError::InvalidTransition("routing generation overflow".into())
            })?;
        Ok(self.routing.generation)
    }

    #[must_use]
    pub fn routing(&self) -> &RoutingGeneration {
        &self.routing
    }

    pub fn status(&self, key: &str) -> ModuleResult<ModuleStatus> {
        let record = self.record(key)?;
        Ok(ModuleStatus {
            key: key.to_owned(),
            name: record.manifest.name.clone(),
            kind: record.manifest.kind,
            version: record.manifest.version.clone(),
            artifact_digest: record.manifest.artifact_digest.clone(),
            state: record.state,
            conformance: record.conformance.clone(),
        })
    }

    #[must_use]
    pub fn list(&self) -> Vec<ModuleStatus> {
        self.modules
            .keys()
            .filter_map(|key| self.status(key).ok())
            .collect()
    }

    fn record(&self, key: &str) -> ModuleResult<&ModuleRecord> {
        self.modules
            .get(key)
            .ok_or_else(|| ModuleError::NotInstalled(key.to_owned()))
    }

    fn record_mut(&mut self, key: &str) -> ModuleResult<&mut ModuleRecord> {
        self.modules
            .get_mut(key)
            .ok_or_else(|| ModuleError::NotInstalled(key.to_owned()))
    }
}

fn module_key(manifest: &ModuleManifest) -> String {
    format!("{}@{}", manifest.name, manifest.version)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SignedBootstrapSnapshot {
    pub snapshot: BootstrapSnapshot,
    pub signature: SignatureEnvelope,
}

#[must_use]
pub fn bootstrap_signing_message(snapshot: &BootstrapSnapshot) -> Vec<u8> {
    let canonical = serde_json::to_vec(snapshot).expect("BootstrapSnapshot always serializes");
    let digest = sha256_digest(&canonical);
    let mut message = Vec::with_capacity(BOOTSTRAP_DOMAIN.len() + digest.len());
    message.extend_from_slice(BOOTSTRAP_DOMAIN);
    message.extend_from_slice(digest.as_bytes());
    message
}

pub fn verify_bootstrap_snapshot(
    signed: &SignedBootstrapSnapshot,
    trust: &TrustStore,
    now_unix_millis: i64,
) -> ModuleResult<()> {
    if signed.snapshot.schema_version != 1
        || signed.snapshot.generated_at_unix_millis > now_unix_millis
    {
        return Err(ModuleError::BootstrapNotYetValid);
    }
    if signed.snapshot.expires_at_unix_millis <= now_unix_millis {
        return Err(ModuleError::BootstrapExpired);
    }
    let mut root_ids = BTreeSet::new();
    for root in &signed.snapshot.trust_roots {
        let key =
            hex::decode(&root.public_key_hex).map_err(|_| ModuleError::InvalidSignatureEncoding)?;
        if root.algorithm != "ed25519"
            || key.len() != 32
            || root.fingerprint != sha256_digest(&key)
            || !root_ids.insert(root.key_id.as_str())
        {
            return Err(ModuleError::InvalidSignatureEncoding);
        }
    }
    let mut provider_names = BTreeSet::new();
    for provider in &signed.snapshot.providers {
        validated_digest_hex(&provider.artifact_digest)?;
        validated_digest_hex(&provider.config_digest)?;
        if !provider_names.insert(provider.name.as_str())
            || !(provider.endpoint.starts_with("unix:") || provider.endpoint.starts_with("https:"))
        {
            return Err(ModuleError::InvalidManifest(
                "invalid bootstrap provider".to_owned(),
            ));
        }
    }
    let signature_bytes = hex::decode(&signed.signature.signature_hex)
        .map_err(|_| ModuleError::InvalidSignatureEncoding)?;
    let public_key = trust.verifying_key(&signed.signature)?;
    signature::UnparsedPublicKey::new(&signature::ED25519, public_key)
        .verify(
            &bootstrap_signing_message(&signed.snapshot),
            &signature_bytes,
        )
        .map_err(|_| ModuleError::InvalidSignature)
}

#[derive(Clone, Debug)]
pub struct BootstrapStore {
    path: PathBuf,
}

impl BootstrapStore {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn save(&self, snapshot: &SignedBootstrapSnapshot) -> ModuleResult<()> {
        let encoded = serde_json::to_vec(snapshot)
            .map_err(|error| ModuleError::BootstrapEncoding(error.to_string()))?;
        let parent = self.path.parent().ok_or_else(|| {
            ModuleError::BootstrapEncoding("snapshot path has no parent".to_owned())
        })?;
        fs::create_dir_all(parent)?;
        let temporary = unique_temporary_path(&self.path);
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        let mut file = options.open(&temporary)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(&encoded)?;
        file.sync_all()?;
        fs::rename(&temporary, &self.path)?;
        Ok(())
    }

    pub fn load(
        &self,
        trust: &TrustStore,
        now_unix_millis: i64,
    ) -> ModuleResult<SignedBootstrapSnapshot> {
        let metadata = fs::symlink_metadata(&self.path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(ModuleError::BootstrapEncoding(
                "snapshot path is not a regular file".to_owned(),
            ));
        }
        let encoded = fs::read(&self.path)?;
        let signed: SignedBootstrapSnapshot = serde_json::from_slice(&encoded)
            .map_err(|error| ModuleError::BootstrapEncoding(error.to_string()))?;
        verify_bootstrap_snapshot(&signed, trust, now_unix_millis)?;
        Ok(signed)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesiredPlacement {
    pub module_key: String,
    pub artifact_digest: String,
    pub required_hosts: BTreeSet<String>,
    pub quorum: usize,
    pub routing_generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedPlacement {
    pub host: String,
    pub module_key: String,
    pub artifact_digest: String,
    pub verified: bool,
    pub ready: bool,
    pub routing_generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReconcileAction {
    Install { host: String },
    Stage { host: String },
    AdvanceRouting { generation: u64 },
}

pub fn reconcile_cluster(
    desired: &DesiredPlacement,
    observed: &[ObservedPlacement],
) -> ModuleResult<Vec<ReconcileAction>> {
    if desired.quorum == 0 || desired.quorum > desired.required_hosts.len() {
        return Err(ModuleError::PlacementQuorum);
    }
    let mut actions = Vec::new();
    let mut ready = 0_usize;
    for host in &desired.required_hosts {
        let placement = observed.iter().find(|item| {
            &item.host == host
                && item.module_key == desired.module_key
                && item.artifact_digest == desired.artifact_digest
        });
        match placement {
            None => actions.push(ReconcileAction::Install { host: host.clone() }),
            Some(item) if !item.verified => {
                actions.push(ReconcileAction::Install { host: host.clone() });
            }
            Some(item) if !item.ready => {
                actions.push(ReconcileAction::Stage { host: host.clone() });
            }
            Some(_) => ready += 1,
        }
    }
    if ready >= desired.quorum {
        let needs_advance = observed.iter().filter(|item| item.ready).any(|item| {
            item.routing_generation < desired.routing_generation
                && desired.required_hosts.contains(&item.host)
        });
        if needs_advance {
            actions.push(ReconcileAction::AdvanceRouting {
                generation: desired.routing_generation,
            });
        }
    }
    Ok(actions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseman_contracts::module::{BootstrapTrustRoot, ModulePlatform, ProtocolVersion};
    use ring::signature::KeyPair;
    use std::sync::{Arc, Mutex};

    const SEED: [u8; 32] = [7; 32];

    fn signing_key() -> signature::Ed25519KeyPair {
        signature::Ed25519KeyPair::from_seed_unchecked(&SEED).unwrap()
    }

    fn trust_store() -> TrustStore {
        let key = signing_key();
        let mut trust = TrustStore::default();
        trust
            .enroll(TrustRoot {
                key_id: "publisher-1".to_owned(),
                public_key: key.public_key().as_ref().to_vec(),
                fingerprint: sha256_digest(key.public_key().as_ref()),
                revoked: false,
            })
            .unwrap();
        trust
    }

    fn signature_for_artifact(bytes: &[u8], manifest_source: &[u8]) -> SignatureEnvelope {
        let artifact_digest = sha256_digest(bytes);
        signature_for_binding(&artifact_digest, manifest_source)
    }

    fn signature_for_binding(artifact_digest: &str, manifest_source: &[u8]) -> SignatureEnvelope {
        let manifest_digest = sha256_digest(manifest_source);
        SignatureEnvelope {
            algorithm: "ed25519".to_owned(),
            key_id: "publisher-1".to_owned(),
            signature_hex: hex::encode(
                signing_key()
                    .sign(&artifact_signing_message(artifact_digest, &manifest_digest))
                    .as_ref(),
            ),
        }
    }

    fn manifest(bytes: &[u8], version: &str) -> ModuleManifest {
        ModuleManifest {
            schema_version: 1,
            name: "sample-echo".to_owned(),
            kind: ModuleKind::Sample,
            version: version.to_owned(),
            contract: ">=1.0, <2.0".to_owned(),
            artifact_digest: sha256_digest(bytes),
            command: vec!["/usr/bin/aseman-sample-provider".to_owned()],
            health_endpoint: "/health/ready".to_owned(),
            config_schema: "config.schema.json".to_owned(),
            capabilities: vec!["sample.echo".to_owned()],
            permissions: ModulePermissions::default(),
            platforms: vec![ModulePlatform {
                os: "linux".to_owned(),
                architecture: "x86_64".to_owned(),
            }],
            migrations: Vec::new(),
            sbom: "sbom.spdx.json".to_owned(),
            license_manifest: "licenses.json".to_owned(),
        }
    }

    fn amod_bundle(version: &str) -> Vec<u8> {
        let encode = |path: &str, content: &[u8]| AmodFile {
            path: path.to_owned(),
            content_base64: base64::engine::general_purpose::STANDARD.encode(content),
        };
        let files = vec![
            encode("usr/bin/aseman-sample-provider", b"sample executable"),
            encode("config.schema.json", br#"{"type":"object"}"#),
            encode("sbom.spdx.json", br#"{"spdxVersion":"SPDX-2.3"}"#),
            encode("licenses.json", br#"{"licenses":["MIT"]}"#),
        ];
        let mut manifest = manifest(b"unused", version);
        manifest.artifact_digest = amod_payload_digest(&files).unwrap();
        let manifest_toml = toml::to_string(&manifest).unwrap();
        let signature = signature_for_binding(&manifest.artifact_digest, manifest_toml.as_bytes());
        serde_json::to_vec(&AmodBundle {
            format_version: 1,
            manifest_toml,
            files,
            signature,
        })
        .unwrap()
    }

    #[test]
    fn signature_digest_and_revocation_fail_closed() {
        let bytes = b"harmless sample provider";
        let manifest_source = b"signed manifest";
        let envelope = signature_for_artifact(bytes, manifest_source);
        let verifier = ArtifactVerifier::new(trust_store());
        assert!(
            verifier
                .verify(bytes, manifest_source, &sha256_digest(bytes), &envelope)
                .is_ok()
        );
        assert!(matches!(
            verifier.verify(
                b"substituted",
                manifest_source,
                &sha256_digest(bytes),
                &envelope
            ),
            Err(ModuleError::DigestMismatch)
        ));
        assert!(matches!(
            verifier.verify(
                bytes,
                b"altered permissions",
                &sha256_digest(bytes),
                &envelope
            ),
            Err(ModuleError::InvalidSignature)
        ));

        let mut revoked = trust_store();
        revoked.revoke("publisher-1").unwrap();
        assert!(matches!(
            ArtifactVerifier::new(revoked).verify(
                bytes,
                manifest_source,
                &sha256_digest(bytes),
                &envelope
            ),
            Err(ModuleError::RevokedSigner(_))
        ));
    }

    #[test]
    fn amod_verification_binds_manifest_payload_and_required_files() {
        let bundle_bytes = amod_bundle("1.0.0");
        let verifier = ArtifactVerifier::new(trust_store());
        let (manifest, artifact) = verifier.verify_amod(&bundle_bytes).unwrap();
        assert_eq!(manifest.name, "sample-echo");
        assert_ne!(artifact.digest, artifact.cache_digest);

        let mut payload_tamper: AmodBundle = serde_json::from_slice(&bundle_bytes).unwrap();
        payload_tamper.files[0].content_base64 =
            base64::engine::general_purpose::STANDARD.encode(b"substituted executable");
        assert!(matches!(
            verifier.verify_amod(&serde_json::to_vec(&payload_tamper).unwrap()),
            Err(ModuleError::DigestMismatch)
        ));

        let mut manifest_tamper: AmodBundle = serde_json::from_slice(&bundle_bytes).unwrap();
        let mut changed = parse_manifest(&manifest_tamper.manifest_toml).unwrap();
        changed
            .permissions
            .secret_refs
            .push("new-secret".to_owned());
        manifest_tamper.manifest_toml = toml::to_string(&changed).unwrap();
        assert!(matches!(
            verifier.verify_amod(&serde_json::to_vec(&manifest_tamper).unwrap()),
            Err(ModuleError::InvalidSignature)
        ));

        let mut missing: AmodBundle = serde_json::from_slice(&bundle_bytes).unwrap();
        missing.files.retain(|file| file.path != "sbom.spdx.json");
        let mut changed = parse_manifest(&missing.manifest_toml).unwrap();
        changed.artifact_digest = amod_payload_digest(&missing.files).unwrap();
        missing.manifest_toml = toml::to_string(&changed).unwrap();
        missing.signature =
            signature_for_binding(&changed.artifact_digest, missing.manifest_toml.as_bytes());
        assert!(matches!(
            verifier.verify_amod(&serde_json::to_vec(&missing).unwrap()),
            Err(ModuleError::InvalidManifest(_))
        ));
    }

    #[test]
    fn manifest_rejects_permission_widening_and_path_escape() {
        let bytes = b"sample";
        let mut value = manifest(bytes, "1.0.0");
        value.permissions.read_only_mounts = vec!["../host".to_owned()];
        assert!(validate_manifest(&value).is_err());
        value.permissions.read_only_mounts = vec!["/data".to_owned()];
        value.permissions.read_write_mounts = vec!["/data".to_owned()];
        assert!(validate_manifest(&value).is_err());
    }

    #[test]
    fn negotiation_requires_major_kind_capability_and_bounds() {
        let handshake = ModuleHandshake {
            protocol: ProtocolVersion { major: 1, minor: 3 },
            provider_kind: ModuleKind::Sample,
            implementation_version: "1.0.0".to_owned(),
            instance_id: "instance-a".to_owned(),
            capabilities: vec!["sample.echo".to_owned(), "sample.health".to_owned()],
            schema_digests: vec!["sha256:abc".to_owned()],
            max_message_bytes: 4096,
        };
        let mut provider = handshake.clone();
        provider.protocol.minor = 1;
        provider.max_message_bytes = 1024;
        let required = BTreeSet::from(["sample.echo".to_owned()]);
        let negotiated = negotiate(&handshake, &provider, &required).unwrap();
        assert_eq!(negotiated.protocol_minor, 1);
        assert_eq!(negotiated.max_message_bytes, 1024);
        provider.protocol.major = 2;
        assert!(negotiate(&handshake, &provider, &required).is_err());
    }

    struct SampleSuite;

    impl ConformanceSuite for SampleSuite {
        fn validate(
            &self,
            manifest: &ModuleManifest,
            artifact: &Path,
        ) -> ModuleResult<ConformanceReport> {
            if !manifest
                .capabilities
                .iter()
                .any(|item| item == "sample.echo")
                || !artifact.exists()
            {
                return Err(ModuleError::Conformance("sample echo unavailable".into()));
            }
            Ok(ConformanceReport {
                suite_version: "module-v1".to_owned(),
                passed_cases: vec!["health".to_owned(), "echo".to_owned()],
            })
        }
    }

    #[derive(Default)]
    struct ProcessState {
        starts: usize,
        drains: usize,
        stops: usize,
    }

    struct SampleProcess(Arc<Mutex<ProcessState>>);

    impl ModuleProcess for SampleProcess {
        fn start(&mut self) -> ModuleResult<()> {
            self.0.lock().unwrap().starts += 1;
            Ok(())
        }
        fn ready(&mut self) -> ModuleResult<bool> {
            Ok(true)
        }
        fn restore(&mut self) -> ModuleResult<()> {
            Ok(())
        }
        fn drain(&mut self) -> ModuleResult<()> {
            self.0.lock().unwrap().drains += 1;
            Ok(())
        }
        fn stop(&mut self) -> ModuleResult<()> {
            self.0.lock().unwrap().stops += 1;
            Ok(())
        }
    }

    struct SampleFactory(Arc<Mutex<ProcessState>>);

    impl ModuleProcessFactory for SampleFactory {
        fn spawn(&self, launch: ModuleLaunch) -> ModuleResult<SpawnedModule> {
            assert!(launch.artifact_path.exists());
            Ok(SpawnedModule {
                process: Box::new(SampleProcess(self.0.clone())),
                enforced_permissions: launch.permissions,
                injected_secret_refs: BTreeSet::new(),
            })
        }
    }

    #[test]
    fn sample_provider_installs_validates_switches_drains_and_rolls_back() {
        let temp = std::env::temp_dir().join(format!("aseman-module-test-{}", std::process::id()));
        if temp.exists() {
            fs::remove_dir_all(&temp).unwrap();
        }
        let verifier = ArtifactVerifier::new(trust_store());
        let cache = ArtifactCache::new(&temp).unwrap();
        let mut supervisor = ModuleSupervisor::new(verifier, cache);
        let state = Arc::new(Mutex::new(ProcessState::default()));
        let factory = SampleFactory(state.clone());

        let v1 = amod_bundle("1.0.0");
        let key1 = supervisor.install_amod(&v1).unwrap();
        supervisor.validate(&key1, &SampleSuite).unwrap();
        supervisor.stage(&key1, &factory).unwrap();
        assert_eq!(supervisor.activate(&key1).unwrap(), 1);

        let v2 = amod_bundle("1.1.0");
        let key2 = supervisor.install_amod(&v2).unwrap();
        supervisor.validate(&key2, &SampleSuite).unwrap();
        supervisor.stage(&key2, &factory).unwrap();
        assert_eq!(supervisor.activate(&key2).unwrap(), 2);
        assert_eq!(
            supervisor.status(&key1).unwrap().state,
            ModuleLifecycleState::Draining
        );
        supervisor.drain(&key1).unwrap();
        assert_eq!(supervisor.rollback(ModuleKind::Sample).unwrap(), 3);
        assert_eq!(
            supervisor.status(&key1).unwrap().state,
            ModuleLifecycleState::Active
        );
        assert_eq!(state.lock().unwrap().starts, 2);
        assert_eq!(state.lock().unwrap().drains, 1);

        let mut secret_bundle: AmodBundle = serde_json::from_slice(&amod_bundle("1.2.0")).unwrap();
        let mut secret_manifest = parse_manifest(&secret_bundle.manifest_toml).unwrap();
        secret_manifest
            .permissions
            .secret_refs
            .push("sample-token".to_owned());
        secret_bundle.manifest_toml = toml::to_string(&secret_manifest).unwrap();
        secret_bundle.signature = signature_for_binding(
            &secret_manifest.artifact_digest,
            secret_bundle.manifest_toml.as_bytes(),
        );
        let key3 = supervisor
            .install_amod(&serde_json::to_vec(&secret_bundle).unwrap())
            .unwrap();
        supervisor.validate(&key3, &SampleSuite).unwrap();
        assert!(matches!(
            supervisor.stage(&key3, &factory),
            Err(ModuleError::PermissionEnforcement(_))
        ));
        assert_eq!(state.lock().unwrap().starts, 2);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn signed_bootstrap_rejects_expiry_and_tampering() {
        let snapshot = BootstrapSnapshot {
            schema_version: 1,
            node_id: "node-a".to_owned(),
            routing_generation: 9,
            generated_at_unix_millis: 100,
            expires_at_unix_millis: 200,
            providers: Vec::new(),
            trust_roots: vec![BootstrapTrustRoot {
                key_id: "publisher-1".to_owned(),
                algorithm: "ed25519".to_owned(),
                public_key_hex: hex::encode(signing_key().public_key().as_ref()),
                fingerprint: sha256_digest(signing_key().public_key().as_ref()),
            }],
        };
        let signed = SignedBootstrapSnapshot {
            signature: SignatureEnvelope {
                algorithm: "ed25519".to_owned(),
                key_id: "publisher-1".to_owned(),
                signature_hex: hex::encode(
                    signing_key()
                        .sign(&bootstrap_signing_message(&snapshot))
                        .as_ref(),
                ),
            },
            snapshot,
        };
        assert!(verify_bootstrap_snapshot(&signed, &trust_store(), 150).is_ok());
        let directory =
            std::env::temp_dir().join(format!("aseman-bootstrap-test-{}", std::process::id()));
        if directory.exists() {
            fs::remove_dir_all(&directory).unwrap();
        }
        let store = BootstrapStore::new(directory.join("module-bootstrap.json"));
        store.save(&signed).unwrap();
        assert_eq!(
            store
                .load(&trust_store(), 150)
                .unwrap()
                .snapshot
                .routing_generation,
            9
        );
        assert!(matches!(
            verify_bootstrap_snapshot(&signed, &trust_store(), 200),
            Err(ModuleError::BootstrapExpired)
        ));
        let mut tampered = signed.clone();
        tampered.snapshot.routing_generation = 10;
        assert!(matches!(
            verify_bootstrap_snapshot(&tampered, &trust_store(), 150),
            Err(ModuleError::InvalidSignature)
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cluster_routing_advances_only_after_quorum() {
        let desired = DesiredPlacement {
            module_key: "sample@1.0.0".to_owned(),
            artifact_digest: "sha256:abc".to_owned(),
            required_hosts: BTreeSet::from(["a".to_owned(), "b".to_owned(), "c".to_owned()]),
            quorum: 2,
            routing_generation: 4,
        };
        let observed = [
            ObservedPlacement {
                host: "a".to_owned(),
                module_key: desired.module_key.clone(),
                artifact_digest: desired.artifact_digest.clone(),
                verified: true,
                ready: true,
                routing_generation: 3,
            },
            ObservedPlacement {
                host: "b".to_owned(),
                module_key: desired.module_key.clone(),
                artifact_digest: desired.artifact_digest.clone(),
                verified: true,
                ready: true,
                routing_generation: 3,
            },
        ];
        let actions = reconcile_cluster(&desired, &observed).unwrap();
        assert!(actions.contains(&ReconcileAction::Install {
            host: "c".to_owned()
        }));
        assert!(actions.contains(&ReconcileAction::AdvanceRouting { generation: 4 }));
    }

    #[test]
    fn checked_in_manifest_fixtures_match_the_validator() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/module/fixtures");
        let valid = fs::read_to_string(root.join("valid/sample-module.toml")).unwrap();
        parse_manifest(&valid).unwrap();
        for name in ["path-escape.toml", "permission-conflict.toml"] {
            let invalid = fs::read_to_string(root.join("invalid").join(name)).unwrap();
            assert!(parse_manifest(&invalid).is_err(), "accepted {name}");
        }
    }
}
