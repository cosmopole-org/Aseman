//! The bootstrapped compact deployment: where it lives, and its lifecycle.
//!
//! `asemanctl bootstrap` writes `compact.env` (image references, ports, and paths)
//! into its configuration directory and drives the compact Compose profile. Every
//! later command finds the same deployment here, so `status`, `start`, `stop`, and the
//! A902 drivers act on exactly what bootstrap brought up.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use aseman_fs::{Access, write_atomic};

use aseman_config::CliConfig;

use super::args;

/// The profile an installed release ships, relative to the binary
/// (`<prefix>/bin/asemanctl` → `<prefix>/share/aseman/compose/`).
const INSTALLED_COMPOSE: &str = "../share/aseman/compose/compact.compose.yaml";
/// The same profile in a source checkout.
const SOURCE_COMPOSE: &str = "deploy/compose/compact.compose.yaml";
/// The host-local node health port when the deployment did not choose one.
pub(crate) const DEFAULT_HEALTH_PORT: u16 = 8080;

/// The state directory: `--state-dir`, else the configured one
/// ([`CliConfig::state_dir`]).
pub(crate) fn state_dir(arguments: &[String], config: &CliConfig) -> PathBuf {
    args::flag_value(arguments, "state-dir").map_or_else(|| config.state_dir.clone(), PathBuf::from)
}

/// The compact Compose profile: `--compose-file`, else the installed copy beside this
/// binary, else the source checkout's.
pub(crate) fn default_compose_file(arguments: &[String]) -> PathBuf {
    if let Some(file) = args::flag_value(arguments, "compose-file") {
        return PathBuf::from(file);
    }
    let installed = std::env::current_exe()
        .ok()
        .and_then(|executable| executable.parent().map(|dir| dir.join(INSTALLED_COMPOSE)))
        .filter(|path| path.is_file());
    installed.unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_default()
            .join(SOURCE_COMPOSE)
    })
}

/// The VMM backend a compact deployment runs (`ASEMAN_BACKEND` in `compact.env`,
/// Nomad when absent): one Compose profile, service, and configuration file each.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Backend {
    /// Workloads on the operator's Nomad (ADR 0002).
    Nomad,
    /// The native backend's runtime plugins (Modal and the in-process runtimes).
    Native,
}

impl Backend {
    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "nomad" => Ok(Self::Nomad),
            "native" => Ok(Self::Native),
            other => bail!("unknown backend {other:?}; use nomad or native"),
        }
    }

    /// The Compose profile that enables it.
    pub(crate) const fn profile(self) -> &'static str {
        match self {
            Self::Nomad => "nomad",
            Self::Native => "native",
        }
    }

    pub(crate) const fn service(self) -> &'static str {
        match self {
            Self::Nomad => "nomad-backend",
            Self::Native => "native-backend",
        }
    }

    /// Its configuration file, relative to the configuration directory.
    pub(crate) const fn config_file(self) -> &'static str {
        match self {
            Self::Nomad => "nomad-backend.json",
            Self::Native => "native-backend.json",
        }
    }

    pub(crate) const fn default_image(self) -> &'static str {
        match self {
            Self::Nomad => "aseman-vmm-backend-nomad:local",
            Self::Native => "aseman-vmm-backend-native:local",
        }
    }
}

/// The `compact.env` key naming the backend image (older deployments name it
/// `ASEMAN_NOMAD_BACKEND_IMAGE`, which the profile still accepts).
pub(crate) const BACKEND_IMAGE_KEY: &str = "ASEMAN_BACKEND_IMAGE";

/// A compact deployment bootstrap produced.
#[derive(Clone, Debug)]
pub(crate) struct Compact {
    pub(crate) env_file: PathBuf,
    pub(crate) compose_file: PathBuf,
}

impl Compact {
    /// The deployment under `--config-dir` (else `<state>/compact`), if bootstrap
    /// created one.
    pub(crate) fn locate(arguments: &[String], config: &CliConfig) -> Option<Self> {
        let config_dir = args::flag_value(arguments, "config-dir")
            .map(PathBuf::from)
            .unwrap_or_else(|| state_dir(arguments, config).join("compact"));
        let env_file = config_dir.join("compact.env");
        env_file.is_file().then(|| Self {
            env_file,
            compose_file: default_compose_file(arguments),
        })
    }

    /// Like [`Compact::locate`], but an absent deployment is an error.
    pub(crate) fn require(arguments: &[String], config: &CliConfig) -> Result<Self> {
        Self::locate(arguments, config).ok_or_else(|| {
            anyhow!(
                "no compact deployment found; run `asemanctl bootstrap --profile compact` first"
            )
        })
    }

    /// The backend this deployment runs.
    pub(crate) fn backend(&self) -> Result<Backend> {
        self.env_value("ASEMAN_BACKEND")?
            .map_or(Ok(Backend::Nomad), |value| Backend::parse(&value))
    }

    fn command(&self, arguments: &[&str]) -> Result<Command> {
        let mut command = Command::new("docker");
        command
            .args(["compose", "--env-file"])
            .arg(&self.env_file)
            .arg("-f")
            .arg(&self.compose_file)
            .args(["--profile", self.backend()?.profile()])
            .args(arguments);
        Ok(command)
    }

    /// Run `docker compose …` against this deployment and fail on a non-zero exit.
    pub(crate) fn compose(&self, arguments: &[&str]) -> Result<Output> {
        let output = self
            .command(arguments)?
            .output()
            .context("run docker compose")?;
        if !output.status.success() {
            bail!(
                "docker compose {} failed: {}",
                arguments.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(output)
    }

    /// Run a program inside the `postgres` service, streaming `stdin` from and `stdout`
    /// to files, so dumps never pass through memory or the host network.
    pub(crate) fn exec_postgres(
        &self,
        program: &[&str],
        stdin: Option<&Path>,
        stdout: Option<&Path>,
    ) -> Result<Output> {
        let mut arguments = vec!["exec", "-T", "postgres"];
        arguments.extend_from_slice(program);
        let mut command = self.command(&arguments)?;
        if let Some(path) = stdin {
            command.stdin(Stdio::from(
                fs::File::open(path).with_context(|| format!("open {}", path.display()))?,
            ));
        }
        if let Some(path) = stdout {
            command.stdout(Stdio::from(
                fs::File::create(path).with_context(|| format!("create {}", path.display()))?,
            ));
        }
        let output = command.output().context("run docker compose exec")?;
        if !output.status.success() {
            bail!(
                "{} failed in the postgres service: {}",
                program.first().copied().unwrap_or("command"),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(output)
    }

    /// One `KEY=value` from `compact.env`.
    pub(crate) fn env_value(&self, key: &str) -> Result<Option<String>> {
        let text = fs::read_to_string(&self.env_file)
            .with_context(|| format!("read {}", self.env_file.display()))?;
        Ok(text.lines().find_map(|line| {
            line.strip_prefix(key)
                .and_then(|rest| rest.strip_prefix('='))
                .map(str::to_owned)
        }))
    }

    /// Replace (or append) `KEY=value` in `compact.env`, keeping its owner-only mode.
    pub(crate) fn set_env_value(&self, key: &str, value: &str) -> Result<()> {
        let text = fs::read_to_string(&self.env_file)?;
        let mut replaced = false;
        let mut lines: Vec<String> = text
            .lines()
            .map(|line| {
                if line.starts_with(&format!("{key}=")) {
                    replaced = true;
                    format!("{key}={value}")
                } else {
                    line.to_owned()
                }
            })
            .collect();
        if !replaced {
            lines.push(format!("{key}={value}"));
        }
        let contents = lines.join("\n") + "\n";
        write_atomic(&self.env_file, contents.as_bytes(), Access::Private)
            .with_context(|| format!("write {}", self.env_file.display()))
    }

    /// The host-local port the node publishes health on.
    pub(crate) fn health_port(&self) -> u16 {
        self.env_value("ASEMAN_HEALTH_PORT")
            .ok()
            .flatten()
            .and_then(|value| value.parse().ok())
            .unwrap_or(DEFAULT_HEALTH_PORT)
    }

    /// Whether the node answers its health endpoint.
    pub(crate) fn node_healthy(&self) -> bool {
        Command::new("curl")
            .args(["--fail", "--silent", "--max-time", "3"])
            .arg(format!(
                "http://127.0.0.1:{}/telemetry/health",
                self.health_port()
            ))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// Wait up to `timeout` for the node to report healthy.
    pub(crate) fn wait_healthy(&self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if self.node_healthy() {
                return true;
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        self.node_healthy()
    }
}

/// `asemanctl status` — every compact service's state and the node's health.
pub(crate) fn run_status(config: &CliConfig, arguments: &[String]) -> Result<()> {
    let compact = Compact::require(arguments, config)?;
    let output = compact.compose(&["ps", "--all", "--format", "{{.Service}}\t{{.Status}}"])?;
    print!("{}", String::from_utf8_lossy(&output.stdout));
    let healthy = compact.node_healthy();
    println!(
        "node health (127.0.0.1:{}): {}",
        compact.health_port(),
        if healthy { "ok" } else { "unavailable" }
    );
    if !healthy {
        bail!("the node is not healthy");
    }
    Ok(())
}

/// `asemanctl start [SERVICE…]` — start the deployment (or the named services).
pub(crate) fn run_start(config: &CliConfig, arguments: &[String]) -> Result<()> {
    let compact = Compact::require(arguments, config)?;
    let services = services(arguments);
    let mut compose = vec!["up", "-d", "--wait"];
    compose.extend(services.iter().map(String::as_str));
    compact.compose(&compose)?;
    println!("started");
    Ok(())
}

/// `asemanctl stop [SERVICE…]` — stop the deployment (or the named services),
/// keeping every volume.
pub(crate) fn run_stop(config: &CliConfig, arguments: &[String]) -> Result<()> {
    let compact = Compact::require(arguments, config)?;
    let services = services(arguments);
    let mut compose = vec!["stop"];
    compose.extend(services.iter().map(String::as_str));
    compact.compose(&compose)?;
    println!("stopped");
    Ok(())
}

/// Positional service names, skipping flags and their values.
fn services(arguments: &[String]) -> Vec<String> {
    const VALUED: [&str; 3] = ["--state-dir", "--config-dir", "--compose-file"];
    let mut names = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if VALUED.contains(&argument.as_str()) {
            index += 2;
            continue;
        }
        if !argument.starts_with("--") {
            names.push(argument.clone());
        }
        index += 1;
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_names_skip_flags_and_their_values() {
        let arguments: Vec<String> = ["--state-dir", "/s", "node", "--json", "meter"]
            .iter()
            .map(|value| (*value).to_owned())
            .collect();
        assert_eq!(services(&arguments), ["node", "meter"]);
    }

    #[test]
    fn env_values_are_replaced_in_place_and_stay_private() {
        let dir = std::env::temp_dir().join(format!("compact-env-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let env_file = dir.join("compact.env");
        fs::write(&env_file, "A=1\nASEMAN_HEALTH_PORT=18080\n").unwrap();
        let compact = Compact {
            env_file: env_file.clone(),
            compose_file: dir.join("compose.yaml"),
        };
        assert_eq!(compact.health_port(), 18080);
        compact.set_env_value("A", "2").unwrap();
        compact.set_env_value("B", "3").unwrap();
        assert_eq!(
            fs::read_to_string(&env_file).unwrap(),
            "A=2\nASEMAN_HEALTH_PORT=18080\nB=3\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&env_file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
