//! Resumable A602 compact deployment bootstrap (P9-01, A901/A902).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, anyhow, bail};
use aseman_domain::bootstrap::{Progress, Stage, StageOutcome, preflight_passed};
use base64::Engine;
use ring::rand::{SecureRandom, SystemRandom};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const EXPECTED_SERVICES: [&str; 5] = ["postgres", "vmm", "nomad-backend", "node", "meter"];

#[derive(Clone, Debug)]
struct Options {
    profile: String,
    state_dir: PathBuf,
    config_dir: PathBuf,
    compose_file: PathBuf,
    node_image: String,
    vmm_image: String,
    meter_image: String,
    backend_image: String,
    postgres_image: String,
    nomad_endpoint: String,
    allow_unsigned_local: bool,
    plan: bool,
}

impl Options {
    fn parse(arguments: &[String]) -> Result<Self> {
        if arguments
            .iter()
            .any(|value| value == "-h" || value == "--help")
        {
            print_usage();
            return Err(anyhow!("help requested"));
        }
        let root = std::env::current_dir().context("resolve current directory")?;
        let state_dir = aseman_config::cli_config()
            .and_then(|config| config.state_dir.as_deref())
            .map(PathBuf::from)
            .unwrap_or_else(|| aseman_config::process_state_home().join("asemanctl"));
        let mut options = Self {
            profile: "compact".to_owned(),
            config_dir: state_dir.join("compact"),
            state_dir,
            compose_file: root.join("deploy/compose/compact.compose.yaml"),
            node_image: "aseman-node:local".to_owned(),
            vmm_image: "aseman-vmm:local".to_owned(),
            meter_image: "aseman-meter:local".to_owned(),
            backend_image: "aseman-vmm-backend-nomad:local".to_owned(),
            postgres_image: "postgres:18-bookworm".to_owned(),
            nomad_endpoint: "http://host.docker.internal:4646".to_owned(),
            allow_unsigned_local: false,
            plan: false,
        };
        let mut config_explicit = false;
        let mut index = 0;
        while index < arguments.len() {
            let flag = arguments[index].as_str();
            match flag {
                "--allow-unsigned-local" => options.allow_unsigned_local = true,
                "--plan" => options.plan = true,
                "--profile" | "--state-dir" | "--config-dir" | "--compose-file"
                | "--node-image" | "--vmm-image" | "--meter-image" | "--backend-image"
                | "--postgres-image" | "--nomad-endpoint" => {
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
                        "--backend-image" => options.backend_image.clone_from(value),
                        "--postgres-image" => options.postgres_image.clone_from(value),
                        "--nomad-endpoint" => options.nomad_endpoint.clone_from(value),
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
        if options.profile != "compact" {
            bail!(
                "profile {} is not executable yet; use compact (cluster remains A901 work)",
                options.profile
            );
        }
        Ok(options)
    }

    fn progress_file(&self) -> PathBuf {
        self.state_dir.join("bootstrap-compact.json")
    }

    fn env_file(&self) -> PathBuf {
        self.config_dir.join("compact.env")
    }
}

pub fn run_bootstrap(arguments: &[String]) -> Result<()> {
    let options = match Options::parse(arguments) {
        Ok(options) => options,
        Err(error) if error.to_string() == "help requested" => return Ok(()),
        Err(error) => return Err(error),
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
    for service in EXPECTED_SERVICES {
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
        ("nomad backend", &options.backend_image),
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
        return validate_identity(&options.config_dir).with_context(|| {
            format!(
                "validate identity left by an interrupted bootstrap in {}",
                options.config_dir.display()
            )
        });
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
    Ok(())
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
    write_secret(
        &root.join("nomad-backend.json"),
        &serde_json::to_string_pretty(&serde_json::json!({
            "endpoint": options.nomad_endpoint,
            "namespace": "aseman",
            "datacenters": ["dc1"],
            "runtimes": {"docker": {}},
            "network": {"denies_egress": false},
            "timeout_millis": 30000
        }))?,
    )?;
    write_secret(
        &root.join("postgres-init/001-roles.sql"),
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'aseman_guest_proxy') THEN CREATE ROLE aseman_guest_proxy NOLOGIN; END IF; END $$;\n",
    )?;
    let env = format!(
        "ASEMAN_CONFIG_DIR={}\nASEMAN_NODE_ID={}\nASEMAN_NODE_IMAGE={}\nASEMAN_VMM_IMAGE={}\nASEMAN_METER_IMAGE={}\nASEMAN_NOMAD_BACKEND_IMAGE={}\nPOSTGRES_IMAGE={}\nVMM_NODE_CERT_SHA256={}\nVMM_METER_CERT_SHA256={}\nNOMAD_ENDPOINT={}\n",
        options.config_dir.display(),
        uuid::Uuid::now_v7(),
        options.node_image,
        options.vmm_image,
        options.meter_image,
        options.backend_image,
        options.postgres_image,
        node_fingerprint,
        meter_fingerprint,
        options.nomad_endpoint,
    );
    write_secret(&root.join("compact.env"), &env)?;
    fs::remove_dir_all(authority).context("remove bootstrap certificate authority key")?;
    validate_identity(root)?;
    Ok(())
}

fn validate_identity(root: &Path) -> Result<()> {
    const REQUIRED: [&str; 15] = [
        "compact.env",
        "nomad-backend.json",
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
    for relative in REQUIRED {
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
    let missing: Vec<_> = EXPECTED_SERVICES
        .into_iter()
        .filter(|service| !running.contains(*service))
        .collect();
    if !missing.is_empty() {
        bail!("services not running: {}", missing.join(", "));
    }
    checked_output(Command::new("curl").args([
        "--fail",
        "--silent",
        "--show-error",
        "http://127.0.0.1:8080/telemetry/health",
    ]))?;
    Ok(())
}

fn compose(options: &Options, arguments: &[&str]) -> Result<()> {
    compose_output(options, arguments).map(|_| ())
}

fn compose_output(options: &Options, arguments: &[&str]) -> Result<Output> {
    let mut command = Command::new("docker");
    command.args(["compose", "--env-file"]);
    command.arg(options.env_file());
    command.args(["-f"]);
    command.arg(&options.compose_file);
    command.args(arguments);
    checked_output(&mut command)
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
    write_json_atomic(path, progress)
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("state path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".bootstrap-{}.tmp", uuid::Uuid::now_v7()));
    write_secret_bytes(&temporary, &serde_json::to_vec_pretty(value)?)?;
    fs::rename(temporary, path)?;
    Ok(())
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
         Resumes the seven P9-01 stages from an atomic progress file. Release images\n\
         must be digest-pinned; --allow-unsigned-local is development-only. Nomad is\n\
         operator-supplied and is never downloaded.\n\n\
         Options:\n  --state-dir DIR\n  --config-dir DIR\n  --compose-file FILE\n  \
         --node-image IMAGE\n  --vmm-image IMAGE\n  --meter-image IMAGE\n  \
         --backend-image IMAGE\n  --postgres-image IMAGE\n  --nomad-endpoint URL\n  \
         --allow-unsigned-local\n  --plan"
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

    #[test]
    fn release_images_must_be_pinned() {
        let options = Options::parse(&["--plan".to_owned()]).unwrap();
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
        let mut options = Options::parse(&["--plan".to_owned()]).unwrap();
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
        validate_identity(&root).unwrap();
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
        let mut options = Options::parse(&["--plan".to_owned()]).unwrap();
        options.config_dir = root.clone();
        generate_identity(&root, &options).unwrap();
        identity(&options).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
