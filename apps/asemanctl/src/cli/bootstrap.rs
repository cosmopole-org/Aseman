//! Resumable A602 compact deployment bootstrap (A901/A902).

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, anyhow, bail};
use aseman_config::{CliConfig, RuntimeConfig};
use aseman_domain::bootstrap::{Progress, Stage, StageOutcome, preflight_passed};
use aseman_fs::{Access, write_atomic};
use base64::Engine;
use ring::rand::{SecureRandom, SystemRandom};
use sha2::{Digest, Sha256};

use super::compact::{BACKEND_IMAGE_KEY, Backend, Compact};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

/// The host-local port the compact profile publishes node health on by default.
const DEFAULT_HEALTH_PORT: u16 = 8080;

/// The services a compact deployment on `backend` runs.
fn expected_services(backend: Backend) -> [&'static str; 5] {
    ["postgres", "vmm", backend.service(), "node", "meter"]
}

/// Where the native backend's container finds the runtime secrets bootstrap copies
/// into `secrets/runtimes`.
const RUNTIME_SECRETS_MOUNT: &str = "/run/aseman/secrets/runtimes";
/// The runtime secrets' directory, relative to the configuration directory.
const RUNTIME_SECRETS_DIR: &str = "secrets/runtimes";
/// Where the native backend keeps plugin state and artifacts in its container.
const NATIVE_BACKEND_STATE: &str = "/var/lib/aseman-backend";
/// The VM types the compact native backend can run: those that need no host device
/// (the Docker socket and `/dev/kvm` are outside the compact topology, A602).
const COMPACT_NATIVE_VM_TYPES: [&str; 5] = ["modal", "wasm", "elpian", "elpify", "javascript"];

#[derive(Clone, Debug)]
struct Options {
    profile: String,
    state_dir: PathBuf,
    config_dir: PathBuf,
    compose_file: PathBuf,
    node_image: String,
    vmm_image: String,
    meter_image: String,
    backend: Backend,
    backend_image: String,
    postgres_image: String,
    nomad_endpoint: String,
    /// The VM types the native backend serves (`--vm-types`).
    vm_types: Vec<String>,
    /// The native backend's runtime settings, secrets named by their container paths.
    runtime: RuntimeConfig,
    /// Runtime secrets to copy: (file name under `secrets/runtimes`, operator's file).
    runtime_secrets: Vec<(String, PathBuf)>,
    public_port: u16,
    health_port: u16,
    allow_unsigned_local: bool,
    plan: bool,
}

impl Options {
    /// The options, or `None` when help was asked for (and printed).
    fn parse(arguments: &[String], config: &CliConfig) -> Result<Option<Self>> {
        if arguments
            .iter()
            .any(|value| value == "-h" || value == "--help")
        {
            print_usage();
            return Ok(None);
        }
        let state_dir = config.state_dir.clone();
        let mut options = Self {
            profile: "compact".to_owned(),
            config_dir: state_dir.join("compact"),
            state_dir,
            compose_file: super::compact::default_compose_file(arguments),
            node_image: "aseman-node:local".to_owned(),
            vmm_image: "aseman-vmm:local".to_owned(),
            meter_image: "aseman-meter:local".to_owned(),
            backend: Backend::Nomad,
            backend_image: String::new(),
            postgres_image: "postgres:18-bookworm".to_owned(),
            nomad_endpoint: "http://host.docker.internal:4646".to_owned(),
            vm_types: Vec::new(),
            runtime: RuntimeConfig::default(),
            runtime_secrets: Vec::new(),
            public_port: 443,
            health_port: DEFAULT_HEALTH_PORT,
            allow_unsigned_local: false,
            plan: false,
        };
        let mut config_explicit = false;
        let mut backend_image = None;
        let mut nomad_endpoint_explicit = false;
        // The VM types the runtime flags configure, to refuse settings for a type that
        // is not enabled.
        let mut configured_types = BTreeSet::new();
        let mut index = 0;
        while index < arguments.len() {
            let flag = arguments[index].as_str();
            match flag {
                "--allow-unsigned-local" => options.allow_unsigned_local = true,
                "--plan" => options.plan = true,
                "--profile"
                | "--state-dir"
                | "--config-dir"
                | "--compose-file"
                | "--node-image"
                | "--vmm-image"
                | "--meter-image"
                | "--backend-image"
                | "--postgres-image"
                | "--nomad-endpoint"
                | "--public-port"
                | "--health-port"
                | "--backend"
                | "--vm-types"
                | "--vm-http-port"
                | "--modal-api-key-secret"
                | "--modal-token-id-secret"
                | "--modal-token-secret-secret"
                | "--modal-environment"
                | "--modal-server-url"
                | "--modal-client-version"
                | "--modal-app-name"
                | "--modal-app-prefix"
                | "--modal-default-image"
                | "--wasm-aot"
                | "--wasm-vm-cache" => {
                    index += 1;
                    let value = arguments
                        .get(index)
                        .ok_or_else(|| anyhow!("{flag} requires a value"))?;
                    match flag {
                        "--profile" => options.profile.clone_from(value),
                        "--state-dir" => options.state_dir = PathBuf::from(value),
                        "--config-dir" => {
                            options.config_dir = PathBuf::from(value);
                            config_explicit = true;
                        }
                        "--compose-file" => options.compose_file = PathBuf::from(value),
                        "--node-image" => options.node_image.clone_from(value),
                        "--vmm-image" => options.vmm_image.clone_from(value),
                        "--meter-image" => options.meter_image.clone_from(value),
                        "--backend-image" => backend_image = Some(value.clone()),
                        "--postgres-image" => options.postgres_image.clone_from(value),
                        "--nomad-endpoint" => {
                            options.nomad_endpoint.clone_from(value);
                            nomad_endpoint_explicit = true;
                        }
                        "--public-port" => options.public_port = parse_port(flag, value)?,
                        "--health-port" => options.health_port = parse_port(flag, value)?,
                        "--backend" => options.backend = Backend::parse(value)?,
                        "--vm-types" => {
                            options.vm_types = value
                                .split(',')
                                .map(str::trim)
                                .filter(|kind| !kind.is_empty())
                                .map(str::to_owned)
                                .collect();
                        }
                        "--vm-http-port" => {
                            options.runtime.vm_http_port = parse_port(flag, value)?;
                        }
                        "--modal-api-key-secret" => {
                            configured_types.insert("modal");
                            options.runtime.modal_api_key_secret =
                                Some(options.runtime_secret("modal-api-key", flag, value)?);
                        }
                        "--modal-token-id-secret" => {
                            configured_types.insert("modal");
                            options.runtime.modal_token_id_secret =
                                Some(options.runtime_secret("modal-token-id", flag, value)?);
                        }
                        "--modal-token-secret-secret" => {
                            configured_types.insert("modal");
                            options.runtime.modal_token_secret_secret =
                                Some(options.runtime_secret("modal-token-secret", flag, value)?);
                        }
                        "--modal-environment"
                        | "--modal-server-url"
                        | "--modal-client-version"
                        | "--modal-app-name"
                        | "--modal-app-prefix"
                        | "--modal-default-image" => {
                            configured_types.insert("modal");
                            let runtime = &mut options.runtime;
                            match flag {
                                "--modal-environment" => {
                                    runtime.modal_environment.clone_from(value)
                                }
                                "--modal-server-url" => runtime.modal_server_url.clone_from(value),
                                "--modal-client-version" => {
                                    runtime.modal_client_version.clone_from(value);
                                }
                                "--modal-app-name" => runtime.modal_app_name = Some(value.clone()),
                                "--modal-app-prefix" => runtime.modal_app_prefix.clone_from(value),
                                _ => runtime.modal_default_image.clone_from(value),
                            }
                        }
                        "--wasm-aot" | "--wasm-vm-cache" => {
                            configured_types.insert("wasm");
                            let enabled = parse_switch(flag, value)?;
                            if flag == "--wasm-aot" {
                                options.runtime.wasm_aot = enabled;
                            } else {
                                options.runtime.wasm_vm_cache = enabled;
                            }
                        }
                        _ => unreachable!(),
                    }
                }
                _ => bail!("unknown bootstrap argument {flag}"),
            }
            index += 1;
        }
        if !config_explicit {
            options.config_dir = options.state_dir.join("compact");
        }
        options.backend_image =
            backend_image.unwrap_or_else(|| options.backend.default_image().to_owned());
        options.validate_backend(nomad_endpoint_explicit, &configured_types)?;
        if options.profile != "compact" {
            bail!(
                "profile {} is not executable yet; use compact (cluster remains A901 work)",
                options.profile
            );
        }
        Ok(Some(options))
    }

    /// Record the operator's secret file `source` (given with `flag`) to be copied as
    /// `name`; returns the path the native backend reads it at.
    fn runtime_secret(&mut self, name: &str, flag: &str, source: &str) -> Result<PathBuf> {
        let source = PathBuf::from(source);
        if !source.is_file() {
            bail!(
                "{flag} names {}, which is not a readable file",
                source.display()
            );
        }
        self.runtime_secrets.push((name.to_owned(), source));
        Ok(Path::new(RUNTIME_SECRETS_MOUNT).join(name))
    }

    /// The backend's VM types and their settings must agree.
    fn validate_backend(
        &self,
        nomad_endpoint_explicit: bool,
        configured_types: &BTreeSet<&str>,
    ) -> Result<()> {
        match self.backend {
            Backend::Nomad => {
                if !self.vm_types.is_empty() || !configured_types.is_empty() {
                    bail!(
                        "--vm-types and VM settings configure the native backend; add --backend native"
                    );
                }
            }
            Backend::Native => {
                if nomad_endpoint_explicit {
                    bail!("--nomad-endpoint configures the nomad backend, not the native one");
                }
                if self.vm_types.is_empty() {
                    bail!(
                        "the native backend needs --vm-types (any of {})",
                        COMPACT_NATIVE_VM_TYPES.join(", ")
                    );
                }
                for kind in &self.vm_types {
                    if matches!(kind.as_str(), "docker" | "fire") {
                        bail!(
                            "VM type {kind} needs host devices the compact topology does not \
                             grant (the Docker socket or /dev/kvm); run it on a host backend"
                        );
                    }
                    if !COMPACT_NATIVE_VM_TYPES.contains(&kind.as_str()) {
                        bail!(
                            "unknown VM type {kind}; use any of {}",
                            COMPACT_NATIVE_VM_TYPES.join(", ")
                        );
                    }
                }
                for kind in configured_types {
                    if !self.vm_types.iter().any(|enabled| enabled == kind) {
                        bail!("{kind} settings were given but --vm-types does not enable {kind}");
                    }
                }
                let runtime = &self.runtime;
                let modal_credentials = runtime.modal_api_key_secret.is_some()
                    || (runtime.modal_token_id_secret.is_some()
                        && runtime.modal_token_secret_secret.is_some());
                if self.vm_types.iter().any(|kind| kind == "modal") && !modal_credentials {
                    bail!(
                        "the modal VM type needs --modal-api-key-secret (or \
                         --modal-token-id-secret and --modal-token-secret-secret)"
                    );
                }
            }
        }
        Ok(())
    }

    /// The deployment this bootstrap drives.
    fn compact(&self) -> Compact {
        Compact {
            env_file: self.env_file(),
            compose_file: self.compose_file.clone(),
        }
    }

    fn progress_file(&self) -> PathBuf {
        self.state_dir.join("bootstrap-compact.json")
    }

    fn env_file(&self) -> PathBuf {
        self.config_dir.join("compact.env")
    }
}

pub fn run_bootstrap(config: &CliConfig, arguments: &[String]) -> Result<()> {
    let Some(options) = Options::parse(arguments, config)? else {
        return Ok(());
    };
    if options.plan {
        for stage in Stage::ALL {
            println!("{}", stage.as_str());
        }
        return Ok(());
    }

    fs::create_dir_all(&options.state_dir).context("create bootstrap state directory")?;
    let mut progress = read_progress(&options.progress_file())?;
    while let Some(stage) = progress.next() {
        println!("bootstrap: {}", stage.as_str());
        let outcome = execute(stage, &options);
        match outcome {
            Ok(()) => progress.record(stage, &StageOutcome::Done)?,
            Err(error) => {
                progress.record(
                    stage,
                    &StageOutcome::Failed {
                        reason: format!("{error:#}"),
                    },
                )?;
                write_progress(&options.progress_file(), &progress)?;
                return Err(error.context(format!("bootstrap stage {}", stage.as_str())));
            }
        }
        write_progress(&options.progress_file(), &progress)?;
    }
    println!("bootstrap: compact deployment is healthy");
    Ok(())
}

fn execute(stage: Stage, options: &Options) -> Result<()> {
    match stage {
        Stage::Preflight => preflight(options),
        Stage::Topology => topology(options),
        Stage::Artifacts => artifacts(options),
        Stage::Identity => identity(options),
        Stage::Schema => schema(options),
        Stage::Services => services(options),
        Stage::Health => health(options),
    }
}

fn preflight(options: &Options) -> Result<()> {
    let mut findings = Vec::new();
    findings.push(aseman_domain::bootstrap::Finding {
        check: "linux".to_owned(),
        fatal: std::env::consts::OS != "linux",
        detail: std::env::consts::OS.to_owned(),
    });
    findings.push(aseman_domain::bootstrap::Finding {
        check: "cgroup-v2".to_owned(),
        fatal: !Path::new("/sys/fs/cgroup/cgroup.controllers").exists(),
        detail: "/sys/fs/cgroup/cgroup.controllers".to_owned(),
    });
    findings.push(aseman_domain::bootstrap::Finding {
        check: "kvm".to_owned(),
        fatal: false,
        detail: if Path::new("/dev/kvm").exists() {
            "available".to_owned()
        } else {
            "not available; container workloads remain supported".to_owned()
        },
    });
    // A port already bound would make the health gate probe someone else's service.
    for (check, port) in [
        ("public-port", options.public_port),
        ("health-port", options.health_port),
    ] {
        let busy = port_in_use(port);
        findings.push(aseman_domain::bootstrap::Finding {
            check: check.to_owned(),
            fatal: busy,
            detail: if busy {
                format!("127.0.0.1:{port} is already in use; choose another with --{check}")
            } else {
                format!("{port} free")
            },
        });
    }
    for finding in &findings {
        println!(
            "preflight: {}: {}{}",
            finding.check,
            finding.detail,
            if finding.fatal { " (fatal)" } else { "" }
        );
    }
    if !preflight_passed(&findings) {
        bail!("host preflight has fatal findings");
    }
    checked_output(Command::new("docker").args(["compose", "version"]))?;
    checked_output(Command::new("openssl").arg("version"))?;
    if !options.compose_file.is_file() {
        bail!(
            "compose profile {} does not exist",
            options.compose_file.display()
        );
    }
    Ok(())
}

fn topology(options: &Options) -> Result<()> {
    let profile = fs::read_to_string(&options.compose_file)
        .with_context(|| format!("read {}", options.compose_file.display()))?;
    for service in expected_services(options.backend) {
        if !profile.contains(&format!("  {service}:")) {
            bail!("compact profile omits {service}");
        }
    }
    if !profile.contains("network_mode: service:vmm") {
        bail!("compact profile does not keep A504 in the VMM network namespace");
    }
    Ok(())
}

fn artifacts(options: &Options) -> Result<()> {
    for (name, image) in [
        ("node", &options.node_image),
        ("vmm", &options.vmm_image),
        ("meter", &options.meter_image),
        (options.backend.service(), &options.backend_image),
    ] {
        let pinned = image.contains("@sha256:")
            && image.rsplit_once("@sha256:").is_some_and(|(_, digest)| {
                digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            });
        if !pinned && !options.allow_unsigned_local {
            bail!(
                "{name} image {image} is not digest-pinned; use a signed release digest or --allow-unsigned-local for development"
            );
        }
    }
    Ok(())
}

fn identity(options: &Options) -> Result<()> {
    if options.config_dir.exists() {
        validate_identity(&options.config_dir, options.backend).with_context(|| {
            format!(
                "validate identity left by an interrupted bootstrap in {}",
                options.config_dir.display()
            )
        })?;
        return hand_to_runtime(options);
    }
    let parent = options
        .config_dir
        .parent()
        .ok_or_else(|| anyhow!("configuration directory has no parent"))?;
    fs::create_dir_all(parent)?;
    let staging = parent.join(format!(".compact-bootstrap-{}", uuid::Uuid::now_v7()));
    fs::create_dir_all(staging.join("secrets"))?;
    fs::create_dir_all(staging.join("tls"))?;
    fs::create_dir_all(staging.join("postgres-init"))?;
    fs::create_dir_all(staging.join("authority"))?;

    let result = generate_identity(&staging, options);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    fs::rename(&staging, &options.config_dir).context("activate generated configuration")?;
    hand_to_runtime(options)
}

/// The uid:gid every Aseman image runs as (`deploy/images/*.Dockerfile`).
const RUNTIME_OWNER: &str = "65532:65532";

/// Private material each Aseman service reads, relative to the configuration dir,
/// besides the backend's own configuration and runtime secrets.
const RUNTIME_PRIVATE: [&str; 9] = [
    "babble/priv_key",
    "secrets/database-url",
    "secrets/guest-proxy-url",
    "secrets/node-private-key.pem",
    "secrets/node-vmm-identity.pem",
    "secrets/meter-vmm-identity.pem",
    "tls/node-key.pem",
    "tls/vmm-key.pem",
    "tls/meter-key.pem",
];

/// Public material any service may read.
const RUNTIME_PUBLIC: [&str; 7] = [
    "babble/key.pub",
    "babble/peers.genesis.json",
    "tls/ca-cert.pem",
    "tls/node-cert.pem",
    "tls/vmm-cert.pem",
    "tls/meter-cert.pem",
    "postgres-init/001-roles.sql",
];

/// Give each service exactly the files it mounts: private material becomes `0400`
/// owned by the service's own uid (65532 for Aseman images, the image's `postgres`
/// user for the database password), and certificates and the role script become
/// world-readable. Nothing gains group or other access to a secret. Without this the
/// non-root services cannot read their own secrets. Idempotent, so a resumed
/// bootstrap re-applies it.
fn hand_to_runtime(options: &Options) -> Result<()> {
    let root = &options.config_dir;
    for relative in RUNTIME_PUBLIC {
        #[cfg(unix)]
        fs::set_permissions(root.join(relative), fs::Permissions::from_mode(0o644))?;
    }
    let postgres_owner = image_user(&options.postgres_image, "postgres")?;
    let backend_private = std::iter::once(options.backend.config_file().to_owned()).chain(
        options
            .runtime_secrets
            .iter()
            .map(|(name, _)| format!("{RUNTIME_SECRETS_DIR}/{name}")),
    );
    let mut assignments: Vec<(String, String)> = RUNTIME_PRIVATE
        .iter()
        .map(|relative| (*relative).to_owned())
        .chain(backend_private)
        .map(|relative| (relative, RUNTIME_OWNER.to_owned()))
        .collect();
    assignments.push(("secrets/postgres-password".to_owned(), postgres_owner));
    for (relative, _) in &assignments {
        let path = root.join(relative);
        // A file already handed over may no longer be ours to chmod; its mode was set
        // before the transfer.
        #[cfg(unix)]
        if owned_by_operator(&path)? {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o400))?;
        }
    }
    if running_as_root(root)? {
        for (relative, owner) in &assignments {
            let (uid, gid) = parse_owner(owner)?;
            #[cfg(unix)]
            std::os::unix::fs::chown(root.join(relative), Some(uid), Some(gid))
                .with_context(|| format!("hand {relative} to {owner}"))?;
        }
        return Ok(());
    }
    // Docker access is already root-equivalent and required by this profile; a
    // one-shot, network-less container performs the ownership change an unprivileged
    // operator cannot.
    let mut by_owner: std::collections::BTreeMap<&str, Vec<String>> = Default::default();
    for (relative, owner) in &assignments {
        by_owner
            .entry(owner.as_str())
            .or_default()
            .push(format!("/config/{relative}"));
    }
    for (owner, files) in by_owner {
        let mut command = Command::new("docker");
        command
            .args(["run", "--rm", "--network", "none", "--user", "0:0"])
            .args(["--entrypoint", "chown", "--volume"])
            .arg(format!("{}:/config", path_str(root)?))
            .arg(&options.node_image)
            .arg(owner)
            .args(files);
        checked_output(&mut command).context("hand private configuration to the services")?;
    }
    Ok(())
}

/// `uid:gid` of `user` inside `image`.
fn image_user(image: &str, user: &str) -> Result<String> {
    let id = |flag: &str| -> Result<String> {
        let output = checked_output(
            Command::new("docker")
                .args(["run", "--rm", "--network", "none", "--entrypoint", "id"])
                .arg(image)
                .args([flag, user]),
        )?;
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    };
    let owner = format!("{}:{}", id("-u")?, id("-g")?);
    parse_owner(&owner)?;
    Ok(owner)
}

fn parse_owner(owner: &str) -> Result<(u32, u32)> {
    let (uid, gid) = owner
        .split_once(':')
        .ok_or_else(|| anyhow!("invalid owner {owner:?}"))?;
    Ok((uid.parse()?, gid.parse()?))
}

#[cfg(unix)]
fn running_as_root(root: &Path) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    // compact.env stays with the operator, so its owner is the user running bootstrap.
    Ok(fs::metadata(root.join("compact.env"))?.uid() == 0)
}

#[cfg(unix)]
fn owned_by_operator(path: &Path) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let operator = fs::metadata(path.parent().unwrap_or(path))?.uid();
    Ok(fs::metadata(path)?.uid() == operator)
}

fn generate_identity(root: &Path, options: &Options) -> Result<()> {
    let secrets = root.join("secrets");
    let tls = root.join("tls");
    let authority = root.join("authority");
    fs::create_dir_all(&authority)?;
    let password = random_secret()?;
    write_secret(&secrets.join("postgres-password"), &password)?;
    write_secret(
        &secrets.join("database-url"),
        &format!("postgresql://aseman:{password}@postgres:5432/aseman"),
    )?;
    write_secret(
        &secrets.join("guest-proxy-url"),
        &format!("postgresql://aseman:{password}@postgres:5432/postgres"),
    )?;
    openssl(&[
        "genpkey",
        "-algorithm",
        "RSA",
        "-pkeyopt",
        "rsa_keygen_bits:3072",
        "-out",
        path_str(&secrets.join("node-private-key.pem"))?,
    ])?;
    generate_consensus_identity(root, options)?;
    openssl(&[
        "req",
        "-x509",
        "-newkey",
        "rsa:3072",
        "-nodes",
        "-days",
        "30",
        "-subj",
        "/CN=Aseman Compact CA",
        "-keyout",
        path_str(&authority.join("ca-key.pem"))?,
        "-out",
        path_str(&tls.join("ca-cert.pem"))?,
    ])?;
    let ca_key = authority.join("ca-key.pem");
    issue_certificate(
        &tls,
        &ca_key,
        "node",
        "DNS:node,DNS:localhost,IP:127.0.0.1",
        true,
    )?;
    issue_certificate(
        &tls,
        &ca_key,
        "vmm",
        "DNS:vmm,DNS:localhost,IP:127.0.0.1",
        false,
    )?;
    issue_certificate(&tls, &ca_key, "meter", "DNS:meter", false)?;
    combine_identity(
        &tls.join("node-cert.pem"),
        &tls.join("node-key.pem"),
        &secrets.join("node-vmm-identity.pem"),
    )?;
    combine_identity(
        &tls.join("meter-cert.pem"),
        &tls.join("meter-key.pem"),
        &secrets.join("meter-vmm-identity.pem"),
    )?;
    let node_fingerprint = certificate_fingerprint(&tls.join("node-cert.pem"))?;
    let meter_fingerprint = certificate_fingerprint(&tls.join("meter-cert.pem"))?;
    write_backend_config(root, options)?;
    write_secret(
        &root.join("postgres-init/001-roles.sql"),
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'aseman_guest_proxy') THEN CREATE ROLE aseman_guest_proxy NOLOGIN; END IF; END $$;\n",
    )?;
    let env = format!(
        "ASEMAN_CONFIG_DIR={}\nASEMAN_NODE_ID={}\nASEMAN_NODE_IMAGE={}\nASEMAN_VMM_IMAGE={}\nASEMAN_METER_IMAGE={}\nASEMAN_BACKEND={}\nCOMPOSE_PROFILES={}\n{}={}\nPOSTGRES_IMAGE={}\nVMM_NODE_CERT_SHA256={}\nVMM_METER_CERT_SHA256={}\nNOMAD_ENDPOINT={}\nASEMAN_PUBLIC_PORT={}\nASEMAN_HEALTH_PORT={}\n",
        options.config_dir.display(),
        uuid::Uuid::now_v7(),
        options.node_image,
        options.vmm_image,
        options.meter_image,
        options.backend.profile(),
        options.backend.profile(),
        BACKEND_IMAGE_KEY,
        options.backend_image,
        options.postgres_image,
        node_fingerprint,
        meter_fingerprint,
        options.nomad_endpoint,
        options.public_port,
        options.health_port,
    );
    write_secret(&root.join("compact.env"), &env)?;
    fs::remove_dir_all(authority).context("remove bootstrap certificate authority key")?;
    validate_identity(root, options.backend)?;
    Ok(())
}

/// The backend's configuration file: the operator's Nomad for the nomad backend; the
/// enabled VM types, their settings, and their copied secrets for the native one.
fn write_backend_config(root: &Path, options: &Options) -> Result<()> {
    let config = match options.backend {
        Backend::Nomad => serde_json::json!({
            "endpoint": options.nomad_endpoint,
            "namespace": "aseman",
            "datacenters": ["dc1"],
            "runtimes": {"docker": {}},
            "network": {"denies_egress": false},
            "timeout_millis": 30000
        }),
        Backend::Native => {
            let secrets = root.join(RUNTIME_SECRETS_DIR);
            fs::create_dir_all(&secrets)?;
            for (name, source) in &options.runtime_secrets {
                let value = aseman_config::read_secret_file(source, 4096)
                    .map_err(|error| anyhow!("read {}: {error}", source.display()))?;
                write_secret(&secrets.join(name), value.trim())?;
            }
            serde_json::json!({
                "state_dir": NATIVE_BACKEND_STATE,
                "node_ca": "/etc/aseman/tls/ca-cert.pem",
                "vm_types": options.vm_types,
                "runtime": options.runtime,
            })
        }
    };
    write_secret(
        &root.join(options.backend.config_file()),
        &serde_json::to_string_pretty(&config)?,
    )
}

/// The loopback address the compact node's single-validator Hashgraph chain gossips
/// on; it is never published, and the genesis names it as the only peer.
const COMPACT_CONSENSUS_ADDRESS: &str = "127.0.0.1:1337";

/// The legacy Hashgraph validator identity the node's consensus provider needs
///: a key pair from the image's own `aseman-keygen`, generated as the
/// operator, and a genesis naming this node as the chain's only peer — the same
/// material `asemanctl install --local` produces.
fn generate_consensus_identity(root: &Path, options: &Options) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let babble = root.join("babble");
    let home = root.join("authority").join("keygen-home");
    fs::create_dir_all(&babble)?;
    fs::create_dir_all(&home)?;
    let metadata = fs::metadata(root)?;
    checked_output(
        Command::new("docker")
            .args(["run", "--rm", "--network", "none", "--user"])
            .arg(format!("{}:{}", metadata.uid(), metadata.gid()))
            .args(["--env", "HOME=/keygen", "--volume"])
            .arg(format!("{}:/keygen", path_str(&home)?))
            .args(["--entrypoint", "/usr/local/bin/aseman-keygen"])
            .arg(&options.node_image),
    )
    .context("generate the consensus validator key")?;
    let generated = home.join(".babble");
    let private = fs::read(generated.join("priv_key")).context("read generated priv_key")?;
    write_secret_bytes(&babble.join("priv_key"), &private)?;
    let public = fs::read_to_string(generated.join("key.pub"))
        .context("read generated key.pub")?
        .split_whitespace()
        .collect::<String>();
    if public.is_empty() || !public.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("the generated consensus public key is not hex");
    }
    fs::write(babble.join("key.pub"), &public)?;
    let genesis = serde_json::json!([{
        "NetAddr": COMPACT_CONSENSUS_ADDRESS,
        "PubKeyHex": format!("0X{}", public.to_uppercase()),
        "Moniker": "compact-node",
    }]);
    fs::write(
        babble.join("peers.genesis.json"),
        serde_json::to_vec(&genesis)?,
    )?;
    fs::remove_dir_all(&home).context("remove key generation scratch")?;
    Ok(())
}

fn validate_identity(root: &Path, backend: Backend) -> Result<()> {
    const REQUIRED: [&str; 17] = [
        "compact.env",
        "babble/priv_key",
        "babble/key.pub",
        "babble/peers.genesis.json",
        "postgres-init/001-roles.sql",
        "secrets/postgres-password",
        "secrets/database-url",
        "secrets/guest-proxy-url",
        "secrets/node-private-key.pem",
        "secrets/node-vmm-identity.pem",
        "secrets/meter-vmm-identity.pem",
        "tls/ca-cert.pem",
        "tls/node-cert.pem",
        "tls/node-key.pem",
        "tls/vmm-cert.pem",
        "tls/vmm-key.pem",
        "tls/meter-cert.pem",
    ];
    for relative in REQUIRED.into_iter().chain([backend.config_file()]) {
        let path = root.join(relative);
        if !path.is_file() || fs::metadata(&path)?.len() == 0 {
            bail!("generated identity file {relative} is missing or empty");
        }
    }
    if root.join("tls/ca-key.pem").exists() || root.join("authority").exists() {
        bail!("certificate authority private material was not removed");
    }
    certificate_fingerprint(&root.join("tls/node-cert.pem"))?;
    certificate_fingerprint(&root.join("tls/vmm-cert.pem"))?;
    certificate_fingerprint(&root.join("tls/meter-cert.pem"))?;
    Ok(())
}

fn schema(options: &Options) -> Result<()> {
    compose(options, &["up", "-d", "--wait", "postgres"])?;
    compose(
        options,
        &[
            "exec",
            "-T",
            "postgres",
            "pg_isready",
            "-U",
            "aseman",
            "-d",
            "aseman",
        ],
    )?;
    // Init scripts that fail (for example an unreadable file) do not stop the
    // container, so the stage checks the roles they provision.
    let output = compose_output(
        options,
        &[
            "exec",
            "-T",
            "postgres",
            "psql",
            "-U",
            "aseman",
            "-d",
            "aseman",
            "-Atc",
            "SELECT count(*) FROM pg_roles WHERE rolname = 'aseman_guest_proxy'",
        ],
    )?;
    if String::from_utf8_lossy(&output.stdout).trim() != "1" {
        bail!("PostgreSQL initialization did not provision the aseman_guest_proxy role");
    }
    Ok(())
}

fn services(options: &Options) -> Result<()> {
    compose(options, &["up", "-d", "--wait"])?;
    Ok(())
}

fn health(options: &Options) -> Result<()> {
    let output = compose_output(options, &["ps", "--status", "running", "--services"])?;
    let service_list = String::from_utf8(output.stdout)?;
    let running: std::collections::BTreeSet<String> = service_list
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    let missing: Vec<_> = expected_services(options.backend)
        .into_iter()
        .filter(|service| !running.contains(*service))
        .collect();
    if !missing.is_empty() {
        bail!("services not running: {}", missing.join(", "));
    }
    // The port the deployment was generated with, so a resumed run probes this
    // deployment's node and never another service on the default port.
    let health_port = options
        .compact()
        .env_value("ASEMAN_HEALTH_PORT")?
        .map(|value| parse_port("ASEMAN_HEALTH_PORT", &value))
        .transpose()?
        .unwrap_or(DEFAULT_HEALTH_PORT);
    checked_output(Command::new("curl").args([
        "--fail",
        "--silent",
        "--show-error",
        &format!("http://127.0.0.1:{health_port}/telemetry/health"),
    ]))?;
    Ok(())
}

fn parse_port(flag: &str, value: &str) -> Result<u16> {
    match value.parse::<u16>() {
        Ok(port) if port > 0 => Ok(port),
        _ => bail!("{flag} must be a TCP port, got {value:?}"),
    }
}

fn port_in_use(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(300),
    )
    .is_ok()
}

fn compose(options: &Options, arguments: &[&str]) -> Result<()> {
    compose_output(options, arguments).map(|_| ())
}

fn compose_output(options: &Options, arguments: &[&str]) -> Result<Output> {
    options.compact().compose(arguments)
}

/// An `on`/`off` switch.
fn parse_switch(flag: &str, value: &str) -> Result<bool> {
    match value {
        "on" | "true" | "yes" => Ok(true),
        "off" | "false" | "no" => Ok(false),
        _ => bail!("{flag} must be on or off, got {value:?}"),
    }
}

fn openssl(arguments: &[&str]) -> Result<Output> {
    checked_output(Command::new("openssl").args(arguments))
}

fn issue_certificate(tls: &Path, ca_key: &Path, name: &str, san: &str, first: bool) -> Result<()> {
    let key = tls.join(format!("{name}-key.pem"));
    let csr = tls.join(format!("{name}.csr"));
    let cert = tls.join(format!("{name}-cert.pem"));
    let ca_cert = tls.join("ca-cert.pem");
    openssl(&[
        "req",
        "-newkey",
        "rsa:3072",
        "-nodes",
        "-subj",
        &format!("/CN={name}"),
        "-addext",
        &format!("subjectAltName={san}"),
        "-keyout",
        path_str(&key)?,
        "-out",
        path_str(&csr)?,
    ])?;
    let mut arguments = vec![
        "x509",
        "-req",
        "-in",
        path_str(&csr)?,
        "-CA",
        path_str(&ca_cert)?,
        "-CAkey",
        path_str(ca_key)?,
        "-out",
        path_str(&cert)?,
        "-days",
        "30",
        "-copy_extensions",
        "copy",
    ];
    if first {
        arguments.push("-CAcreateserial");
    }
    openssl(&arguments)?;
    fs::remove_file(csr)?;
    Ok(())
}

fn certificate_fingerprint(certificate: &Path) -> Result<String> {
    let output = openssl(&["x509", "-in", path_str(certificate)?, "-outform", "DER"])?;
    Ok(hex::encode(Sha256::digest(output.stdout)))
}

fn combine_identity(certificate: &Path, key: &Path, output: &Path) -> Result<()> {
    let mut bytes = fs::read(certificate)?;
    bytes.extend_from_slice(&fs::read(key)?);
    write_secret_bytes(output, &bytes)
}

fn random_secret() -> Result<String> {
    let mut bytes = [0_u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow!("operating-system random source failed"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

fn checked_output(command: &mut Command) -> Result<Output> {
    let description = format!("{command:?}");
    let output = command
        .output()
        .with_context(|| format!("start {description}"))?;
    if !output.status.success() {
        bail!(
            "{description} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output)
}

fn read_progress(path: &Path) -> Result<Progress> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("parse bootstrap progress"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Progress::default()),
        Err(error) => Err(error).context("read bootstrap progress"),
    }
}

fn write_progress(path: &Path, progress: &Progress) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("state path has no parent"))?;
    fs::create_dir_all(parent)?;
    write_atomic(path, &serde_json::to_vec_pretty(progress)?, Access::Private)
        .with_context(|| format!("write {}", path.display()))
}

fn write_secret(path: &Path, value: &str) -> Result<()> {
    write_secret_bytes(path, value.as_bytes())
}

fn write_secret_bytes(path: &Path, value: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(value)?;
    file.sync_all()?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow!("path is not valid UTF-8: {}", path.display()))
}

fn print_usage() {
    println!(
        "asemanctl bootstrap --profile compact [options]\n\n\
         Resumes the seven stages from an atomic progress file. Release images\n\
         must be digest-pinned; --allow-unsigned-local is development-only. Nomad is\n\
         operator-supplied and is never downloaded.\n\n\
         Options:\n  --state-dir DIR\n  --config-dir DIR\n  --compose-file FILE\n  \
         --node-image IMAGE\n  --vmm-image IMAGE\n  --meter-image IMAGE\n  \
         --backend-image IMAGE\n  --postgres-image IMAGE\n  \
         --public-port PORT (default 443)\n  --health-port PORT (default 8080, host-local)\n  \
         --allow-unsigned-local\n  --plan\n\n\
         Backend (default nomad):\n  \
         --backend nomad|native\n  \
         --nomad-endpoint URL                (nomad)\n  \
         --vm-types TYPE,...                 (native: {types})\n  \
         --vm-http-port PORT                 (native)\n\n\
         Modal (with --vm-types modal; secrets are files, never values):\n  \
         --modal-api-key-secret FILE         <token-id>:<token-secret>\n  \
         --modal-token-id-secret FILE        with --modal-token-secret-secret FILE\n  \
         --modal-environment NAME\n  --modal-server-url URL\n  --modal-client-version VERSION\n  \
         --modal-app-name NAME\n  --modal-app-prefix PREFIX\n  --modal-default-image IMAGE\n\n\
         Wasm (with --vm-types wasm):\n  --wasm-aot on|off\n  --wasm-vm-cache on|off",
        types = COMPACT_NATIVE_VM_TYPES.join(", ")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_lists_the_domain_stages_in_order() {
        assert_eq!(
            Stage::ALL.map(Stage::as_str),
            [
                "preflight",
                "topology",
                "artifacts",
                "identity",
                "schema",
                "services",
                "health"
            ]
        );
    }

    #[test]
    fn progress_round_trips_and_resumes() {
        let root =
            std::env::temp_dir().join(format!("aseman-bootstrap-test-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("progress.json");
        let mut progress = Progress::default();
        progress
            .record(Stage::Preflight, &StageOutcome::Done)
            .unwrap();
        write_progress(&path, &progress).unwrap();
        let restored = read_progress(&path).unwrap();
        assert_eq!(restored.next(), Some(Stage::Topology));
        fs::remove_dir_all(root).unwrap();
    }

    fn parse(arguments: &[&str]) -> Result<Options> {
        let arguments: Vec<String> = arguments.iter().map(|value| (*value).to_owned()).collect();
        Options::parse(&arguments, &super::super::test_config())
            .map(|options| options.expect("options"))
    }

    fn secret_file(name: &str, value: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("aseman-bootstrap-{name}-{}", uuid::Uuid::now_v7()));
        fs::write(&path, value).unwrap();
        path
    }

    #[test]
    fn the_native_backend_takes_its_vm_types_and_their_settings() {
        let key = secret_file("modal-key", "ak-1:as-1\n");
        let key_arg = key.to_str().unwrap();
        let options = parse(&[
            "--backend",
            "native",
            "--vm-types",
            "modal,wasm",
            "--modal-api-key-secret",
            key_arg,
            "--modal-app-name",
            "prod",
            "--wasm-vm-cache",
            "off",
        ])
        .unwrap();
        assert_eq!(options.backend, Backend::Native);
        assert_eq!(options.backend_image, "aseman-vmm-backend-native:local");
        assert_eq!(options.vm_types, ["modal", "wasm"]);
        assert_eq!(options.runtime.modal_app_name.as_deref(), Some("prod"));
        assert!(!options.runtime.wasm_vm_cache);
        assert_eq!(
            options.runtime.modal_api_key_secret.as_deref(),
            Some(Path::new("/run/aseman/secrets/runtimes/modal-api-key"))
        );

        let root =
            std::env::temp_dir().join(format!("aseman-native-config-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        write_backend_config(&root, &options).unwrap();
        let config: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(root.join("native-backend.json")).unwrap())
                .unwrap();
        assert_eq!(config["vm_types"], serde_json::json!(["modal", "wasm"]));
        assert_eq!(
            config["runtime"]["modal_api_key_secret"],
            "/run/aseman/secrets/runtimes/modal-api-key"
        );
        // The configuration names the secret; only the copied file holds it.
        assert!(
            !fs::read_to_string(root.join("native-backend.json"))
                .unwrap()
                .contains("as-1")
        );
        assert_eq!(
            fs::read_to_string(root.join("secrets/runtimes/modal-api-key")).unwrap(),
            "ak-1:as-1"
        );
        fs::remove_dir_all(root).unwrap();
        fs::remove_file(key).unwrap();
    }

    #[test]
    fn backend_settings_that_do_not_fit_are_refused() {
        let key = secret_file("modal-key-refused", "ak:as");
        let key_arg = key.to_str().unwrap();
        for (arguments, expected) in [
            (vec!["--backend", "native"], "needs --vm-types"),
            (
                vec!["--backend", "native", "--vm-types", "docker"],
                "host devices",
            ),
            (
                vec!["--backend", "native", "--vm-types", "fire"],
                "host devices",
            ),
            (
                vec!["--backend", "native", "--vm-types", "lisp"],
                "unknown VM type",
            ),
            (
                vec!["--backend", "native", "--vm-types", "modal"],
                "--modal-api-key-secret",
            ),
            (
                vec![
                    "--backend",
                    "native",
                    "--vm-types",
                    "wasm",
                    "--modal-api-key-secret",
                    key_arg,
                ],
                "does not enable modal",
            ),
            (vec!["--vm-types", "modal"], "add --backend native"),
            (vec!["--wasm-aot", "off"], "add --backend native"),
            (
                vec![
                    "--backend",
                    "native",
                    "--vm-types",
                    "wasm",
                    "--nomad-endpoint",
                    "http://x",
                ],
                "nomad backend",
            ),
            (
                vec![
                    "--backend",
                    "native",
                    "--vm-types",
                    "modal",
                    "--modal-api-key-secret",
                    "/absent",
                ],
                "not a readable file",
            ),
        ] {
            let error = parse(&arguments)
                .err()
                .unwrap_or_else(|| panic!("{arguments:?} was accepted"));
            assert!(
                error.to_string().contains(expected),
                "{arguments:?}: {error}"
            );
        }
        fs::remove_file(key).unwrap();
    }

    #[test]
    fn release_images_must_be_pinned() {
        let options = Options::parse(&["--plan".to_owned()], &super::super::test_config())
            .unwrap()
            .expect("options");
        assert!(artifacts(&options).is_err());
    }

    #[test]
    fn identity_generation_produces_parseable_certificates_and_no_secret_in_git() {
        let root = std::env::temp_dir().join(format!(
            "aseman-bootstrap-identity-test-{}",
            uuid::Uuid::now_v7()
        ));
        fs::create_dir_all(root.join("secrets")).unwrap();
        fs::create_dir_all(root.join("tls")).unwrap();
        fs::create_dir_all(root.join("postgres-init")).unwrap();
        fs::create_dir_all(root.join("authority")).unwrap();
        let mut options = Options::parse(&["--plan".to_owned()], &super::super::test_config())
            .unwrap()
            .expect("options");
        options.config_dir = root.clone();
        generate_identity(&root, &options).unwrap();
        assert_eq!(
            certificate_fingerprint(&root.join("tls/node-cert.pem"))
                .unwrap()
                .len(),
            64
        );
        let identity = fs::read_to_string(root.join("secrets/node-vmm-identity.pem")).unwrap();
        assert!(identity.contains("BEGIN CERTIFICATE"));
        assert!(identity.contains("BEGIN PRIVATE KEY"));
        assert!(!root.join("tls/ca-key.pem").exists());
        assert!(!root.join("authority").exists());
        validate_identity(&root, Backend::Nomad).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn identity_stage_accepts_a_complete_directory_after_a_crash_boundary() {
        let root = std::env::temp_dir().join(format!(
            "aseman-bootstrap-resume-test-{}",
            uuid::Uuid::now_v7()
        ));
        fs::create_dir_all(root.join("secrets")).unwrap();
        fs::create_dir_all(root.join("tls")).unwrap();
        fs::create_dir_all(root.join("postgres-init")).unwrap();
        fs::create_dir_all(root.join("authority")).unwrap();
        let mut options = Options::parse(&["--plan".to_owned()], &super::super::test_config())
            .unwrap()
            .expect("options");
        options.config_dir = root.clone();
        generate_identity(&root, &options).unwrap();
        identity(&options).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
