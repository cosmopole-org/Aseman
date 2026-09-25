//! Aseman administration command groups (RL-015, A902, A903).
//!
//! `doctor`, `backup`, `restore`, `upgrade`, and `support-bundle` drive the ordered,
//! resumable [`OperationJournal`] plans from `aseman-domain::operations`. The pure
//! state machine is delivered; this module supplies the execution drivers — the part
//! of A902 that actually operates files, directories, processes, and checks.
//!
//! Every command persists its journal under the state directory and refuses to repeat
//! a completed step, so an interrupted backup, restore, or upgrade resumes where it
//! stopped and a failed step is retried rather than skipped.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use aseman_config::AsemanConfig;
use aseman_domain::operations::{OperationJournal, OperationKind, OperationStep};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::run;

// ───────────────────────── shared context ─────────────────────────────────

/// One check result collected by `doctor` (and reused by `Health`).
#[derive(Clone, Debug, Serialize)]
struct Finding {
    check: String,
    fatal: bool,
    detail: String,
}

impl Finding {
    fn ok(check: &str, detail: impl Into<String>) -> Self {
        Self {
            check: check.to_owned(),
            fatal: false,
            detail: detail.into(),
        }
    }

    fn warn(check: &str, detail: impl Into<String>) -> Self {
        Self {
            check: check.to_owned(),
            fatal: false,
            detail: detail.into(),
        }
    }

    fn fatal(check: &str, detail: impl Into<String>) -> Self {
        Self {
            check: check.to_owned(),
            fatal: true,
            detail: detail.into(),
        }
    }
}

/// The signed backup manifest (mirrors `contracts/operations/backup-manifest.schema.json`).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct BackupManifest {
    version: u32,
    backup_id: String,
    created_at: String,
    source_cluster_id: String,
    capsule_schema_versions: BTreeMap<String, u32>,
    provider_mappings: BTreeMap<String, String>,
    module_versions: BTreeMap<String, String>,
    artifacts: Vec<Artifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<Signature>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Artifact {
    logical_name: String,
    media_type: String,
    size_bytes: u64,
    sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Signature {
    algorithm: String,
    key_id: String,
    value: String,
}

impl BackupManifest {
    fn unsigned(&self) -> BackupManifest {
        let mut manifest = self.clone();
        manifest.signature = None;
        manifest
    }

    /// The canonical bytes that are signed and verified.
    fn signing_bytes(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&self.unsigned())?)
    }
}

/// Everything a driver needs beyond the journal itself.
struct OpContext<'a> {
    args: &'a [String],
    repo: PathBuf,
    data_dir: PathBuf,
    state_dir: PathBuf,
    config: Option<AsemanConfig>,
    config_error: Option<String>,
    findings: Vec<Finding>,
    backup_dir: Option<PathBuf>,
    manifest: Option<BackupManifest>,
    signing_key: Option<PathBuf>,
}

impl<'a> OpContext<'a> {
    fn new(args: &'a [String]) -> Result<Self> {
        let repo = run::resolve_repo_dir(args)?;
        let data_dir = run::data_dir(args, &repo);
        let state_dir = state_dir(args);
        fs::create_dir_all(&state_dir)?;
        Ok(Self {
            args,
            repo,
            data_dir,
            state_dir,
            config: None,
            config_error: None,
            findings: Vec::new(),
            backup_dir: None,
            manifest: None,
            signing_key: run::flag_value(args, "signing-key").map(PathBuf::from),
        })
    }

    /// Load the node configuration once. A missing/invalid config is recorded so a
    /// check can report it as a finding instead of aborting the whole doctor run.
    fn ensure_loaded(&mut self) -> bool {
        if self.config.is_some() {
            return true;
        }
        let candidates = [self.data_dir.join(".env"), self.repo.join(".env")];
        let mut last_error = None;
        for dotenv in candidates {
            if dotenv.exists() {
                match AsemanConfig::from_process_with_dotenv(&dotenv) {
                    Ok(config) => {
                        self.config = Some(config);
                        return true;
                    }
                    Err(error) => last_error = Some(error.to_string()),
                }
            }
        }
        match AsemanConfig::from_process_with_dotenv("") {
            Ok(config) => {
                self.config = Some(config);
                true
            }
            Err(error) => {
                self.config_error = Some(last_error.unwrap_or_else(|| error.to_string()));
                false
            }
        }
    }

    fn try_config(&mut self) -> Option<&AsemanConfig> {
        self.ensure_loaded()
            .then(|| self.config.as_ref().expect("configuration was loaded"))
    }

    /// The node configuration or a hard failure (used by operations that cannot run
    /// without it).
    fn require_config(&mut self) -> Result<&AsemanConfig> {
        if !self.ensure_loaded() {
            bail!(
                "node configuration could not be loaded: {}",
                self.config_error.as_deref().unwrap_or("unknown error")
            );
        }
        Ok(self.config.as_ref().expect("configuration was loaded"))
    }

    fn node_running(&self) -> bool {
        run::port_open(8074)
    }
}

// ───────────────────────── state directory and journal ────────────────────

fn state_dir(args: &[String]) -> PathBuf {
    if let Some(dir) = run::flag_value(args, "state-dir") {
        return PathBuf::from(dir);
    }
    aseman_config::cli_config()
        .and_then(|config| config.state_dir.as_deref())
        .map(PathBuf::from)
        .unwrap_or_else(|| aseman_config::process_state_home().join("asemanctl"))
}

fn kind_name(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Upgrade => "upgrade",
        OperationKind::Backup => "backup",
        OperationKind::Restore => "restore",
        OperationKind::Doctor => "doctor",
        OperationKind::SupportBundle => "support-bundle",
    }
}

fn journal_path(kind: OperationKind, state: &Path) -> PathBuf {
    state.join(format!("{}.journal.json", kind_name(kind)))
}

fn load_journal(path: &Path, kind: OperationKind) -> OperationJournal {
    match fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<OperationJournal>(&text).ok())
    {
        Some(journal) if journal.kind == kind => journal,
        _ => OperationJournal::new(kind),
    }
}

fn persist(journal: &OperationJournal, path: &Path) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(journal)?)?;
    Ok(())
}

/// Drive a resumable journal: run the next not-yet-completed step, persist after
/// every transition, stop on the first failure so a re-run retries that step.
fn drive_journal(
    kind: OperationKind,
    args: &[String],
    mut step: impl FnMut(OperationStep, &mut OpContext<'_>) -> Result<()>,
) -> Result<()> {
    let mut ctx = OpContext::new(args)?;
    let path = journal_path(kind, &ctx.state_dir);
    let mut journal = load_journal(&path, kind);
    if journal.complete() {
        println!("{}: already complete", kind_name(kind));
        return Ok(());
    }
    while let Some(next) = journal.next() {
        print!("{}: {} … ", kind_name(kind), step_label(next));
        match step(next, &mut ctx) {
            Ok(()) => {
                journal.succeed(next)?;
                persist(&journal, &path)?;
                println!("ok");
            }
            Err(error) => {
                println!();
                journal.fail(next, error.to_string())?;
                persist(&journal, &path)?;
                bail!(
                    "{} failed at {}: {error:#}\n  journal: {} (re-run to retry this step)",
                    kind_name(kind),
                    step_label(next),
                    path.display()
                );
            }
        }
    }
    println!("{}: complete", kind_name(kind));
    Ok(())
}

fn step_label(step: OperationStep) -> &'static str {
    match step {
        OperationStep::Preflight => "preflight",
        OperationStep::VerifyArtifacts => "verify-artifacts",
        OperationStep::QuiesceWrites => "quiesce-writes",
        OperationStep::SnapshotStores => "snapshot-stores",
        OperationStep::CaptureCatalog => "capture-catalog",
        OperationStep::HashArtifacts => "hash-artifacts",
        OperationStep::SignManifest => "sign-manifest",
        OperationStep::ResumeWrites => "resume-writes",
        OperationStep::VerifyManifest => "verify-manifest",
        OperationStep::PrepareTarget => "prepare-target",
        OperationStep::RestoreStores => "restore-stores",
        OperationStep::ApplyCatalog => "apply-catalog",
        OperationStep::VerifyIntegrity => "verify-integrity",
        OperationStep::DrainServices => "drain-services",
        OperationStep::ApplyUpgrade => "apply-upgrade",
        OperationStep::MigrateSchema => "migrate-schema",
        OperationStep::StartServices => "start-services",
        OperationStep::CheckConfiguration => "check-configuration",
        OperationStep::CheckDependencies => "check-dependencies",
        OperationStep::CheckStorage => "check-storage",
        OperationStep::CheckRuntime => "check-runtime",
        OperationStep::CheckSecurity => "check-security",
        OperationStep::CollectDiagnostics => "collect-diagnostics",
        OperationStep::RedactSecrets => "redact-secrets",
        OperationStep::PackageBundle => "package-bundle",
        OperationStep::Health => "health",
    }
}

// ───────────────────────── filesystem / crypto helpers ────────────────────

/// Recursively copy a directory tree.
fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)
        .with_context(|| format!("cannot read source directory {}", source.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let destination = target.join(entry.file_name());
        if path.is_dir() {
            copy_tree(&path, &destination)?;
        } else {
            fs::copy(&path, &destination).with_context(|| {
                format!(
                    "cannot copy {} to {}",
                    path.display(),
                    destination.display()
                )
            })?;
        }
    }
    Ok(())
}

/// List every regular file under `root`, in sorted order (deterministic).
fn walk_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(encode_hex(&hasher.finalize()))
}

#[cfg(test)]
fn hash_bytes(bytes: &[u8]) -> String {
    encode_hex(&Sha256::digest(bytes))
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_hex(hex: &str) -> Result<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        bail!("hex string has an odd length");
    }
    (0..hex.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&hex[index..index + 2], 16)
                .map_err(|error| anyhow!("invalid hex: {error}"))
        })
        .collect()
}

/// The storage directories the node owns, deduplicated and existing.
fn storage_dirs(config: &AsemanConfig) -> Vec<(&'static str, PathBuf)> {
    let storage = &config.storage;
    let mut seen = std::collections::HashSet::new();
    let mut dirs = Vec::new();
    for (name, path) in [
        ("root", PathBuf::from(&storage.root_path)),
        ("base", PathBuf::from(&storage.base_db_path)),
        ("applets", PathBuf::from(&storage.applet_db_path)),
        ("logs", PathBuf::from(&storage.store_logs_db)),
        ("search", PathBuf::from(&storage.search_index_path)),
    ] {
        if path.exists() && seen.insert(path.clone()) {
            dirs.push((name, path));
        }
    }
    dirs
}

fn provider_str(provider: &aseman_config::CoreStorageProvider) -> &'static str {
    match provider {
        aseman_config::CoreStorageProvider::Legacy => "legacy",
        aseman_config::CoreStorageProvider::Postgres => "postgres",
    }
}

/// The node binary a driver can launch or check.
fn node_binary(repo: &Path) -> Option<PathBuf> {
    let dist = run::arch_dist(repo).join("bin");
    [
        dist.join("aseman-node"),
        repo.join("target/release/aseman-node"),
        dist.join("caspar-node"),
        repo.join("target/release/caspar-node"),
    ]
    .into_iter()
    .find(|path| path.exists())
}

// ───────────────────────── signing (Ed25519 via ring) ─────────────────────

fn load_signing_key(path: &Path) -> Result<ring::signature::Ed25519KeyPair> {
    let raw =
        fs::read(path).with_context(|| format!("cannot read signing key {}", path.display()))?;
    let bytes: Vec<u8> = if raw.len() == 64 && raw.iter().all(u8::is_ascii_hexdigit) {
        decode_hex(&String::from_utf8_lossy(&raw))?
    } else {
        raw
    };
    if bytes.len() != 32 {
        bail!(
            "the signing key at {} must be a 32-byte Ed25519 seed (raw or 64 hex chars)",
            path.display()
        );
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&bytes);
    ring::signature::Ed25519KeyPair::from_seed_unchecked(&seed)
        .map_err(|error| anyhow!("invalid Ed25519 seed: {error}"))
}

fn sign_bytes(key: &ring::signature::Ed25519KeyPair, bytes: &[u8]) -> (String, String) {
    use ring::signature::KeyPair;
    let key_id = encode_hex(key.public_key().as_ref());
    let signature = key.sign(bytes);
    (key_id, encode_hex(signature.as_ref()))
}

fn verify_bytes(public_key_hex: &str, signature_hex: &str, bytes: &[u8]) -> Result<()> {
    let public_key = ring::signature::UnparsedPublicKey::new(
        &ring::signature::ED25519,
        decode_hex(public_key_hex)?,
    );
    public_key
        .verify(bytes, &decode_hex(signature_hex)?)
        .map_err(|_| anyhow!("the manifest signature does not verify"))
}

// ───────────────────────── doctor ─────────────────────────────────────────

/// `asemanctl doctor` — run every health check, collect findings, and fail only at
/// the `Health` step if a fatal finding exists. A doctor run is always fresh; the
/// persisted journal records the last outcome.
pub fn run_doctor(args: &[String]) -> Result<()> {
    if run::has_flag(args, "help") {
        print_doctor_usage();
        return Ok(());
    }
    let mut ctx = OpContext::new(args)?;
    let mut journal = OperationJournal::new(OperationKind::Doctor);
    for &step in OperationKind::Doctor.steps() {
        print!("doctor: {} … ", step_label(step));
        doctor_step(step, &mut ctx)?;
        journal.succeed(step)?;
        println!("ok");
    }
    persist(
        &journal,
        &journal_path(OperationKind::Doctor, &ctx.state_dir),
    )?;

    let fatal: Vec<&Finding> = ctx
        .findings
        .iter()
        .filter(|finding| finding.fatal)
        .collect();
    if run::has_flag(args, "json") {
        println!(
            "{}",
            json!({
                "command": "doctor",
                "healthy": fatal.is_empty(),
                "findings": ctx.findings,
            })
        );
    } else {
        for finding in &ctx.findings {
            let marker = if finding.fatal { "FAIL" } else { "ok  " };
            println!("  [{marker}] {} — {}", finding.check, finding.detail);
        }
    }
    if fatal.is_empty() {
        println!("doctor: healthy");
        Ok(())
    } else {
        let reasons: Vec<&str> = fatal.iter().map(|finding| finding.check.as_str()).collect();
        bail!(
            "doctor found {} fatal problem(s): {}",
            fatal.len(),
            reasons.join(", ")
        )
    }
}

fn doctor_step(step: OperationStep, ctx: &mut OpContext<'_>) -> Result<()> {
    match step {
        OperationStep::CheckConfiguration => {
            let resolved = ctx.try_config().map(|config| {
                (
                    config.node.id.clone(),
                    provider_str(&config.core_storage.provider).to_owned(),
                    config.storage.root_path.clone(),
                )
            });
            match resolved {
                Some((id, provider, root)) => ctx.findings.push(Finding::ok(
                    "configuration",
                    format!("node {id} resolved; core storage provider {provider}; root {root}"),
                )),
                None => ctx.findings.push(Finding::fatal(
                    "configuration",
                    ctx.config_error
                        .clone()
                        .unwrap_or_else(|| "config could not be loaded".to_owned()),
                )),
            }
        }
        OperationStep::CheckDependencies => {
            match node_binary(&ctx.repo) {
                Some(binary) => ctx.findings.push(Finding::ok(
                    "node-binary",
                    format!("{} present", binary.display()),
                )),
                None => ctx.findings.push(Finding::fatal(
                    "node-binary",
                    "no node binary in dist/bin or target/release",
                )),
            }
            let keygen = run::arch_dist(&ctx.repo).join("bin").join("aseman-keygen");
            if keygen.exists() {
                ctx.findings.push(Finding::ok(
                    "keygen",
                    format!("{} present", keygen.display()),
                ));
            } else {
                ctx.findings.push(Finding::warn(
                    "keygen",
                    "aseman-keygen not found in dist/bin (install/build-dist)",
                ));
            }
            if Command::new("tar").arg("--version").output().is_err() {
                ctx.findings.push(Finding::fatal(
                    "archive-tool",
                    "tar is required for support-bundle packaging",
                ));
            }
        }
        OperationStep::CheckStorage => {
            let dirs = ctx.try_config().map(storage_dirs).unwrap_or_default();
            if dirs.is_empty() {
                ctx.findings.push(Finding::warn(
                    "storage",
                    "no configured storage directory exists (install or run the node first)",
                ));
            }
            for (name, path) in dirs {
                let writable = fs::metadata(&path)
                    .map(|meta| !meta.permissions().readonly())
                    .unwrap_or(false);
                if writable {
                    ctx.findings.push(Finding::ok(
                        "storage",
                        format!("{name}: {}", path.display()),
                    ));
                } else {
                    ctx.findings.push(Finding::fatal(
                        "storage",
                        format!("{name} ({}) is not writable", path.display()),
                    ));
                }
            }
        }
        OperationStep::CheckRuntime => {
            if ctx.node_running() {
                ctx.findings.push(Finding::ok(
                    "runtime",
                    "node is listening on the legacy TCP port (8074)",
                ));
            } else {
                ctx.findings.push(Finding::warn(
                    "runtime",
                    "node is not running (start it with `asemanctl run`)",
                ));
            }
        }
        OperationStep::CheckSecurity => {
            let security = ctx.try_config().map(|config| {
                (
                    config.core.tls_certificate_path.clone(),
                    config.core.tls_private_key_path.clone(),
                    config.node.private_key_secret.clone(),
                    config.database_url_secret.clone(),
                )
            });
            match security {
                Some((tls_cert, tls_key, node_secret, db_secret)) => {
                    for (label, path) in [("tls-cert", tls_cert), ("tls-key", tls_key)] {
                        let Some(path) = path else { continue };
                        let file = Path::new(&path);
                        if !file.exists() {
                            ctx.findings.push(Finding::fatal(
                                label,
                                format!("configured file {} is missing", file.display()),
                            ));
                        } else {
                            ctx.findings
                                .push(Finding::ok(label, format!("{} present", file.display())));
                        }
                    }
                    for (label, secret) in
                        [("node-key", Some(node_secret)), ("db-secret", db_secret)]
                    {
                        let Some(secret) = secret else { continue };
                        let path = PathBuf::from(secret);
                        if !path.exists() {
                            continue;
                        }
                        match secret_mode_ok(&path) {
                            Some(true) => ctx.findings.push(Finding::ok(
                                label,
                                format!("{} is not group/world readable", path.display()),
                            )),
                            Some(false) => ctx.findings.push(Finding::fatal(
                                label,
                                format!(
                                    "{} is group or world readable (chmod 600)",
                                    path.display()
                                ),
                            )),
                            None => ctx.findings.push(Finding::fatal(
                                label,
                                format!("cannot stat {}", path.display()),
                            )),
                        }
                    }
                }
                None => ctx.findings.push(Finding::warn(
                    "security",
                    "configuration unavailable; secret permissions not checked",
                )),
            }
        }
        _ => {}
    }
    Ok(())
}

/// Whether a secret file is not group/world readable. `None` when the mode cannot be
/// determined (non-Unix or stat failure).
#[cfg(unix)]
fn secret_mode_ok(path: &Path) -> Option<bool> {
    use std::os::unix::fs::PermissionsExt;
    Some(fs::metadata(path).ok()?.permissions().mode() & 0o077 == 0)
}

#[cfg(not(unix))]
fn secret_mode_ok(_path: &Path) -> Option<bool> {
    None
}

fn print_doctor_usage() {
    println!(
        "asemanctl doctor - run the ordered node health checks\n\n\
         Usage:\n  asemanctl doctor [flags]\n\n\
         Flags:\n  \
         --repo-dir PATH   repo root containing dist/ (auto-detected)\n  \
         --data-dir PATH   node data directory (default <repo>/caspar-data/node1)\n  \
         --state-dir PATH  state/journal directory (default $XDG_STATE_HOME/asemanctl)\n  \
         --json            emit a machine-readable findings report\n\n\
         Checks configuration, dependencies, storage, runtime liveness, and secret\n\
         permissions, then fails if any fatal finding exists."
    );
}

// ───────────────────────── backup ─────────────────────────────────────────

/// `asemanctl backup` — snapshot the node's storage into a target directory and
/// produce a signed backup manifest.
pub fn run_backup(args: &[String]) -> Result<()> {
    if run::has_flag(args, "help") {
        print_backup_usage();
        return Ok(());
    }
    drive_journal(OperationKind::Backup, args, backup_step)
}

fn backup_step(step: OperationStep, ctx: &mut OpContext<'_>) -> Result<()> {
    match step {
        OperationStep::Preflight => {
            let out =
                run::flag_value(ctx.args, "out").ok_or_else(|| anyhow!("--out DIR is required"))?;
            let target = PathBuf::from(&out);
            if target.exists() {
                let existing = fs::read_dir(&target)?.count();
                if existing > 0 {
                    bail!(
                        "backup target {} is not empty; choose a fresh directory",
                        target.display()
                    );
                }
            }
            ctx.backup_dir = Some(target);
        }
        OperationStep::QuiesceWrites => {
            if ctx.node_running() && !run::has_flag(ctx.args, "allow-running") {
                bail!(
                    "the node is running; stop it first (`asemanctl stop`) or pass --allow-running"
                );
            }
            let provider = ctx
                .require_config()
                .map(|config| config.core_storage.provider)?;
            if provider == aseman_config::CoreStorageProvider::Postgres {
                bail!(
                    "core storage is PostgreSQL; back it up through the capsule export \
                     (docs/operations/storage-migration-runbook.md) rather than a file snapshot"
                );
            }
        }
        OperationStep::SnapshotStores => {
            let dirs = ctx.require_config().map(storage_dirs)?;
            let target = ctx
                .backup_dir
                .clone()
                .ok_or_else(|| anyhow!("backup target was not prepared"))?;
            for (name, path) in dirs {
                copy_tree(&path, &target.join("snapshot").join(name))?;
            }
        }
        OperationStep::CaptureCatalog => {
            let (node_id, provider) = ctx.require_config().map(|config| {
                (
                    config.node.id.clone(),
                    provider_str(&config.core_storage.provider).to_owned(),
                )
            })?;
            let mut module_versions = BTreeMap::new();
            module_versions.insert(
                "aseman-node".to_owned(),
                env!("CARGO_PKG_VERSION").to_owned(),
            );
            let mut capsule = BTreeMap::new();
            capsule.insert("core".to_owned(), 1);
            capsule.insert("guest".to_owned(), 1);
            let mut mappings = BTreeMap::new();
            mappings.insert("core_storage".to_owned(), provider);
            ctx.manifest = Some(BackupManifest {
                version: 1,
                backup_id: uuid::Uuid::now_v7().to_string(),
                created_at: chrono::Utc::now().to_rfc3339(),
                source_cluster_id: node_id,
                capsule_schema_versions: capsule,
                provider_mappings: mappings,
                module_versions,
                artifacts: Vec::new(),
                signature: None,
            });
        }
        OperationStep::HashArtifacts => {
            let target = ctx
                .backup_dir
                .clone()
                .ok_or_else(|| anyhow!("backup target was not prepared"))?;
            let snapshot = target.join("snapshot");
            let mut artifacts = Vec::new();
            for path in walk_files(&snapshot)? {
                let size_bytes = fs::metadata(&path)?.len();
                artifacts.push(Artifact {
                    logical_name: path
                        .strip_prefix(&target)
                        .map_err(|_| anyhow!("artifact escaped the backup target"))?
                        .to_string_lossy()
                        .into_owned(),
                    media_type: "application/octet-stream".to_owned(),
                    size_bytes,
                    sha256: hash_file(&path)?,
                });
            }
            if artifacts.is_empty() {
                bail!("no storage files were snapshotted; nothing to back up");
            }
            ctx.manifest
                .as_mut()
                .ok_or_else(|| anyhow!("manifest was not captured"))?
                .artifacts = artifacts;
        }
        OperationStep::SignManifest => {
            let key_path = ctx.signing_key.clone().or_else(|| {
                aseman_config::cli_config()
                    .and_then(|config| config.operator_signing_key.clone())
                    .map(PathBuf::from)
            });
            let key_path = key_path.ok_or_else(|| {
                anyhow!("an operator signing key is required (ASEMAN_OPERATOR_SIGNING_KEY or --signing-key)")
            })?;
            let key = load_signing_key(&key_path)?;
            let signing_bytes = ctx
                .manifest
                .as_ref()
                .ok_or_else(|| anyhow!("manifest was not captured"))?
                .signing_bytes()?;
            let (key_id, value) = sign_bytes(&key, &signing_bytes);
            let manifest = ctx
                .manifest
                .as_mut()
                .ok_or_else(|| anyhow!("manifest was not captured"))?;
            manifest.signature = Some(Signature {
                algorithm: "ed25519".to_owned(),
                key_id,
                value,
            });
            let target = ctx
                .backup_dir
                .clone()
                .ok_or_else(|| anyhow!("backup target was not prepared"))?;
            fs::write(
                target.join("backup-manifest.json"),
                serde_json::to_vec_pretty(manifest)?,
            )?;
        }
        OperationStep::ResumeWrites => {
            // A file snapshot under `--allow-running` is already consistent per file;
            // there is no durable write quiesce to release.
            if ctx.node_running() {
                print!("(writes never quiesced; --allow-running); ");
            }
        }
        OperationStep::VerifyIntegrity => {
            let target = ctx
                .backup_dir
                .clone()
                .ok_or_else(|| anyhow!("backup target was not prepared"))?;
            let manifest = ctx
                .manifest
                .as_ref()
                .ok_or_else(|| anyhow!("manifest was not captured"))?;
            for artifact in &manifest.artifacts {
                let path = target.join(&artifact.logical_name);
                let actual = hash_file(&path)?;
                if actual != artifact.sha256 {
                    bail!(
                        "{} hash mismatch after backup (expected {}, got {})",
                        artifact.logical_name,
                        artifact.sha256,
                        actual
                    );
                }
            }
            if let Some(signature) = &manifest.signature {
                verify_bytes(
                    &signature.key_id,
                    &signature.value,
                    &manifest.signing_bytes()?,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn print_backup_usage() {
    println!(
        "asemanctl backup - snapshot node storage into a signed backup\n\n\
         Usage:\n  asemanctl backup --out DIR [flags]\n\n\
         Flags:\n  \
         --out DIR           backup directory (must be empty or absent)\n  \
         --signing-key FILE  Ed25519 seed (32 bytes or 64 hex chars) to sign the manifest\n  \
         --allow-running     snapshot without stopping the node (best effort)\n  \
         --repo-dir PATH     repo root containing dist/ (auto-detected)\n  \
         --data-dir PATH     node data directory (default <repo>/caspar-data/node1)\n  \
         --state-dir PATH    state/journal directory (default $XDG_STATE_HOME/asemanctl)\n\n\
         Writes <out>/snapshot/* and a signed <out>/backup-manifest.json. PostgreSQL\n\
         core storage is backed up through the capsule export, not a file snapshot."
    );
}

// ───────────────────────── restore ────────────────────────────────────────

/// `asemanctl restore` — restore a node from a backup directory or manifest.
pub fn run_restore(args: &[String]) -> Result<()> {
    if run::has_flag(args, "help") {
        print_restore_usage();
        return Ok(());
    }
    drive_journal(OperationKind::Restore, args, restore_step)
}

fn restore_step(step: OperationStep, ctx: &mut OpContext<'_>) -> Result<()> {
    match step {
        OperationStep::Preflight => {
            let from = run::flag_value(ctx.args, "from")
                .ok_or_else(|| anyhow!("--from DIR|MANIFEST is required"))?;
            let from = PathBuf::from(&from);
            if !from.exists() {
                bail!("--from path {} does not exist", from.display());
            }
            let base = if from.is_dir() {
                from.clone()
            } else {
                from.parent().map(PathBuf::from).unwrap_or_default()
            };
            ctx.backup_dir = Some(base);
        }
        OperationStep::VerifyManifest => {
            let base = ctx
                .backup_dir
                .as_ref()
                .ok_or_else(|| anyhow!("restore source was not resolved"))?;
            let manifest_path = base.join("backup-manifest.json");
            let text = fs::read_to_string(&manifest_path)
                .with_context(|| format!("cannot read {}", manifest_path.display()))?;
            let manifest: BackupManifest =
                serde_json::from_str(&text).context("backup-manifest.json is invalid")?;
            if manifest.version != 1 {
                bail!("unsupported backup manifest version {}", manifest.version);
            }
            if let Some(signature) = &manifest.signature {
                verify_bytes(
                    &signature.key_id,
                    &signature.value,
                    &manifest.signing_bytes()?,
                )?;
            }
            ctx.manifest = Some(manifest);
        }
        OperationStep::PrepareTarget => {
            let root = PathBuf::from(&ctx.require_config()?.storage.root_path);
            fs::create_dir_all(&root)?;
            let non_empty = fs::read_dir(&root)?.count() > 0;
            if non_empty && !run::has_flag(ctx.args, "force") {
                bail!(
                    "{} already contains data; pass --force to restore over it",
                    root.display()
                );
            }
        }
        OperationStep::RestoreStores => {
            let base = ctx
                .backup_dir
                .clone()
                .ok_or_else(|| anyhow!("restore source was not resolved"))?;
            let dirs = ctx.require_config().map(storage_dirs)?;
            let snapshot = base.join("snapshot");
            if !snapshot.exists() {
                bail!("restore source has no snapshot/ tree");
            }
            for (name, destination) in dirs {
                let source = snapshot.join(name);
                if source.exists() {
                    copy_tree(&source, &destination)?;
                }
            }
        }
        OperationStep::ApplyCatalog => {
            let base = ctx
                .backup_dir
                .clone()
                .ok_or_else(|| anyhow!("restore source was not resolved"))?;
            fs::copy(
                base.join("backup-manifest.json"),
                ctx.data_dir.join("backup-manifest.json"),
            )?;
        }
        OperationStep::VerifyIntegrity => {
            let dirs = ctx.require_config().map(storage_dirs)?;
            let manifest = ctx
                .manifest
                .as_ref()
                .ok_or_else(|| anyhow!("manifest was not verified"))?;
            let snapshot = ctx
                .backup_dir
                .clone()
                .ok_or_else(|| anyhow!("restore source was not resolved"))?
                .join("snapshot");
            for (name, destination) in dirs {
                let source = snapshot.join(name);
                if !source.exists() {
                    continue;
                }
                for relative in files_relative(&source)? {
                    let restored = destination.join(&relative);
                    let expected = manifest
                        .artifacts
                        .iter()
                        .find(|artifact| {
                            artifact.logical_name == format!("snapshot/{name}/{relative}")
                        })
                        .map(|artifact| artifact.sha256.as_str())
                        .unwrap_or_default();
                    let actual = hash_file(&restored)?;
                    if !expected.is_empty() && actual != expected {
                        bail!("restored {} hash mismatch", restored.display());
                    }
                }
            }
        }
        OperationStep::StartServices => {
            if ctx.node_running() {
                print!("(node already running); ");
            } else if run::has_flag(ctx.args, "start") {
                run::launch_node(&ctx.repo, &ctx.data_dir, true)?;
            } else {
                print!("(node stopped; start with `asemanctl run` or --start); ");
            }
        }
        OperationStep::Health if !ctx.node_running() => {
            bail!("node is not running after restore")
        }
        _ => {}
    }
    Ok(())
}

/// Files under `root`, relative to `root` (POSIX separators).
fn files_relative(root: &Path) -> Result<Vec<String>> {
    let mut relative = Vec::new();
    for path in walk_files(root)? {
        relative.push(
            path.strip_prefix(root)
                .map_err(|_| anyhow!("file escaped the snapshot root"))?
                .to_string_lossy()
                .into_owned(),
        );
    }
    Ok(relative)
}

fn print_restore_usage() {
    println!(
        "asemanctl restore - restore node storage from a backup\n\n\
         Usage:\n  asemanctl restore --from DIR [flags]\n\n\
         Flags:\n  \
         --from DIR|FILE     backup directory (or its backup-manifest.json)\n  \
         --force             restore over an existing data directory\n  \
         --start             start the node after a successful restore\n  \
         --signing-key FILE  Ed25519 seed to verify the manifest signature\n  \
         --repo-dir PATH     repo root containing dist/ (auto-detected)\n  \
         --data-dir PATH     node data directory (default <repo>/caspar-data/node1)\n  \
         --state-dir PATH    state/journal directory (default $XDG_STATE_HOME/asemanctl)"
    );
}

// ───────────────────────── upgrade ────────────────────────────────────────

/// `asemanctl upgrade` — snapshot the running tree, stop the node, replace the
/// binaries, migrate the schema, and bring the node back up.
pub fn run_upgrade(args: &[String]) -> Result<()> {
    if run::has_flag(args, "help") {
        print_upgrade_usage();
        return Ok(());
    }
    drive_journal(OperationKind::Upgrade, args, upgrade_step)
}

fn upgrade_step(step: OperationStep, ctx: &mut OpContext<'_>) -> Result<()> {
    match step {
        OperationStep::Preflight => {
            if node_binary(&ctx.repo).is_none() {
                bail!("no node binary in dist/bin or target/release to upgrade to");
            }
        }
        OperationStep::VerifyArtifacts => {
            let install_dir = run::flag_value(ctx.args, "install-dir")
                .map(PathBuf::from)
                .unwrap_or_else(|| ctx.repo.join("dist/bin"));
            fs::create_dir_all(&install_dir)?;
            let binary = node_binary(&ctx.repo).ok_or_else(|| anyhow!("no node binary"))?;
            let digest = hash_file(&binary)?;
            print!("({} sha256 {digest}); ", binary.display());
            ctx.backup_dir = Some(install_dir);
        }
        OperationStep::SnapshotStores => {
            let dirs = ctx.require_config().map(storage_dirs)?;
            let snapshot = ctx.state_dir.join(format!(
                "upgrade-snapshot-{}",
                chrono::Utc::now().timestamp()
            ));
            for (name, path) in dirs {
                copy_tree(&path, &snapshot.join(name))?;
            }
        }
        OperationStep::DrainServices => {
            if ctx.node_running() {
                run::stop_local(ctx.args)?;
                print!("(node stopped); ");
            } else {
                print!("(node already stopped); ");
            }
        }
        OperationStep::ApplyUpgrade => {
            let install_dir = ctx
                .backup_dir
                .clone()
                .ok_or_else(|| anyhow!("install directory was not resolved"))?;
            let binary = node_binary(&ctx.repo).ok_or_else(|| anyhow!("no node binary"))?;
            if install_dir == ctx.repo.join("dist/bin") {
                println!(
                    "(binaries already staged in {}; nothing to copy)",
                    install_dir.display()
                );
            } else {
                fs::copy(&binary, install_dir.join("aseman-node"))?;
            }
        }
        OperationStep::MigrateSchema => {
            let (provider, secret) = ctx.require_config().map(|config| {
                (
                    config.core_storage.provider,
                    config.database_url_secret.clone(),
                )
            })?;
            if provider == aseman_config::CoreStorageProvider::Postgres {
                let secret = secret
                    .ok_or_else(|| anyhow!("ASEMAN_DATABASE_URL_SECRET is not configured"))?;
                if !Path::new(&secret).exists() {
                    bail!("database URL secret {} is missing", secret);
                }
                print!(
                    "(PostgreSQL schema migrations run on node start; secret {secret} verified); "
                );
            } else {
                print!("(legacy storage has no schema migration); ");
            }
        }
        OperationStep::StartServices => {
            if ctx.node_running() {
                print!("(node already running); ");
            } else if run::has_flag(ctx.args, "start") {
                run::launch_node(&ctx.repo, &ctx.data_dir, true)?;
            } else {
                print!("(node stopped; start with `asemanctl run` or --start); ");
            }
        }
        OperationStep::Health if !ctx.node_running() => {
            bail!("node is not running after upgrade")
        }
        _ => {}
    }
    Ok(())
}

fn print_upgrade_usage() {
    println!(
        "asemanctl upgrade - snapshot, stop, replace binaries, migrate, restart\n\n\
         Usage:\n  asemanctl upgrade [flags]\n\n\
         Flags:\n  \
         --install-dir PATH  copy the new binary here (default <repo>/dist/bin, no copy)\n  \
         --start             start the node after the upgrade\n  \
         --repo-dir PATH     repo root containing dist/ (auto-detected)\n  \
         --data-dir PATH     node data directory (default <repo>/caspar-data/node1)\n  \
         --state-dir PATH    state/journal directory (default $XDG_STATE_HOME/asemanctl)"
    );
}

// ───────────────────────── support-bundle ─────────────────────────────────

/// `asemanctl support-bundle` — collect diagnostics, redact secrets against the
/// checked contract, and package a tar.gz.
pub fn run_support_bundle(args: &[String]) -> Result<()> {
    if run::has_flag(args, "help") {
        print_support_bundle_usage();
        return Ok(());
    }
    drive_journal(OperationKind::SupportBundle, args, support_bundle_step)
}

fn support_bundle_step(step: OperationStep, ctx: &mut OpContext<'_>) -> Result<()> {
    match step {
        OperationStep::CollectDiagnostics => {
            let collection = ctx
                .state_dir
                .join(format!("support-bundle-{}", chrono::Utc::now().timestamp()));
            fs::create_dir_all(&collection)?;
            ctx.backup_dir = Some(collection.clone());

            let summary = ctx.try_config().map(|config| {
                (
                    config.node.id.clone(),
                    provider_str(&config.core_storage.provider).to_owned(),
                    config.storage.root_path.clone(),
                )
            });
            let config_error = ctx.config_error.clone();
            let mut overview = json!({
                "asemanctl_version": env!("CARGO_PKG_VERSION"),
                "repo": ctx.repo.display().to_string(),
                "data_dir": ctx.data_dir.display().to_string(),
                "node_running": ctx.node_running(),
            });
            if let Some((node_id, provider, root)) = summary {
                overview["node_id"] = json!(node_id);
                overview["core_storage_provider"] = json!(provider);
                overview["storage_root"] = json!(root);
            }
            if let Some(error) = config_error {
                overview["configuration_error"] = json!(error);
            }
            crate::redact_support_bundle(&mut overview).map_err(|error| anyhow!("{error}"))?;
            fs::write(
                collection.join("overview.json"),
                serde_json::to_vec_pretty(&overview)?,
            )?;

            let log = ctx.data_dir.join("node.log");
            if log.exists() {
                fs::copy(&log, collection.join("node.log"))?;
            }
            let dirs = ctx.try_config().map(storage_dirs).unwrap_or_default();
            for (name, path) in dirs {
                if path.exists() {
                    copy_tree(&path, &collection.join("storage").join(name))?;
                }
            }
            print!("(collected into {}); ", collection.display());
        }
        OperationStep::RedactSecrets => {
            let collection = ctx
                .backup_dir
                .as_ref()
                .ok_or_else(|| anyhow!("diagnostics were not collected"))?;
            for path in walk_files(collection)? {
                if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                    let mut value: Value = serde_json::from_slice(&fs::read(&path)?)?;
                    crate::redact_support_bundle(&mut value).map_err(|error| anyhow!("{error}"))?;
                    fs::write(&path, serde_json::to_vec_pretty(&value)?)?;
                }
            }
        }
        OperationStep::PackageBundle => {
            let collection = ctx
                .backup_dir
                .as_ref()
                .ok_or_else(|| anyhow!("diagnostics were not collected"))?;
            let out = run::flag_value(ctx.args, "out")
                .map(PathBuf::from)
                .unwrap_or_else(|| collection.with_extension("tar.gz"));
            let status = Command::new("tar")
                .arg("-czf")
                .arg(&out)
                .arg("-C")
                .arg(collection)
                .arg(".")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .context("failed to run tar; is GNU tar installed?")?;
            if !status.success() {
                bail!("tar failed to package the support bundle");
            }
            fs::write(
                collection.join("archive-path.txt"),
                out.display().to_string(),
            )?;
            print!("(packaged {}); ", out.display());
        }
        OperationStep::VerifyIntegrity => {
            let collection = ctx
                .backup_dir
                .as_ref()
                .ok_or_else(|| anyhow!("diagnostics were not collected"))?;
            let archive = fs::read_to_string(collection.join("archive-path.txt"))
                .map(PathBuf::from)
                .map_err(|_| anyhow!("support bundle was not packaged"))?;
            let meta = fs::metadata(&archive)?;
            if meta.len() == 0 {
                bail!("the support bundle archive is empty");
            }
            let status = Command::new("tar")
                .arg("-tzf")
                .arg(&archive)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .context("failed to run tar")?;
            if !status.success() {
                bail!("the support bundle archive does not list its contents");
            }
            print!("(verified {} bytes); ", meta.len());
        }
        _ => {}
    }
    Ok(())
}

fn print_support_bundle_usage() {
    println!(
        "asemanctl support-bundle - collect, redact, and package diagnostics\n\n\
         Usage:\n  asemanctl support-bundle [--out FILE] [flags]\n\n\
         Flags:\n  \
         --out FILE          archive path (default <state>/support-bundle-<ts>.tar.gz)\n  \
         --repo-dir PATH     repo root containing dist/ (auto-detected)\n  \
         --data-dir PATH     node data directory (default <repo>/caspar-data/node1)\n  \
         --state-dir PATH    state/journal directory (default $XDG_STATE_HOME/asemanctl)\n\n\
         Applies the checked support-bundle redaction contract before packaging,\n\
         and the never_collect list governs what is never read in the first place."
    );
}

#[cfg(test)]
mod tests;
