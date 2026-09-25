//! Canonical Aseman administrative CLI implementation, migrated from Caspar.
//!
//! The Caspar control CLI — full surface ported to Rust:
//!
//! * `install / uninstall / purge` — Docker container lifecycle plus
//!   gVisor runtime install, nginx pull, TLS cert generation, testnet
//!   bootstrap (`prepare-testnet.sh` + `run-testnet.sh`).
//! * `start / pause / resume / stop` — `docker {action}` against the
//!   saved container name.
//! * `stats` — multi-section terminal dashboard with CPU sparkline,
//!   container inspect, telemetry snapshot (incl. **chain stats** from
//!   the running node's telemetry — point (1) of the task brief), and
//!   recent logs. Refreshes every `--interval`.
//! * `pprof` — queries the runtime profiler that `node` now exposes
//!   on `:9999` (Rust `pprof` crate; replacement for Go `net/http/pprof`
//!   — point (3) of the task brief). Subcommands: `runtime`, `heap`,
//!   `threads`, `flamegraph`, `profile`.
//!
//! Implementation notes:
//! - All HTTP queries shell out to `curl` (already required on the host),
//!   keeping the binary tiny.
//! - The Go version used `regexp`/`bufio`/`flag`; we use `regex`/`std`/
//!   `clap` with manual flag parsing where the Go CLI accepts flags **after**
//!   the subcommand (matches `flag.NewFlagSet` behavior).
//! - Signal handling for the stats loop uses the `ctrlc` crate.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

mod cluster;
mod modules;
mod ops;
mod owner;
mod run;
mod vms;
use chrono::{DateTime, Local};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ───────────────────────── Constants & file names ─────────────────────────

const NAME_FILE_NAME: &str = ".casparctl-name";
const IMAGE_FILE_NAME: &str = ".casparctl-image";
const TREND_MAX_POINTS: usize = 30;
const DASHBOARD_WIDTH: usize = 104;

fn docker_name_re() -> &'static Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9_.\-]*$").unwrap())
}

// ───────────────────────── Wire types ─────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DockerStats {
    #[serde(rename = "Container", default)]
    container: String,
    #[serde(rename = "Name", default)]
    name: String,
    #[serde(rename = "ID", default)]
    id: String,
    #[serde(rename = "CPUPerc", default)]
    cpu_perc: String,
    #[serde(rename = "MemUsage", default)]
    mem_usage: String,
    #[serde(rename = "MemPerc", default)]
    mem_perc: String,
    #[serde(rename = "NetIO", default)]
    net_io: String,
    #[serde(rename = "BlockIO", default)]
    block_io: String,
    #[serde(rename = "PIDs", default)]
    pids: String,
}

#[derive(Debug, Clone, Default)]
struct InspectInfo {
    name: String,
    image: String,
    state: String,
    health: String,
    started_at: String,
    created_at: String,
    restart_count: i64,
    ports: Vec<String>,
    mounts: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TelemetrySnapshot {
    #[serde(default)]
    timestamp: String,
    #[serde(default)]
    uptime_sec: i64,
    #[serde(default)]
    node: HashMap<String, Value>,
    #[serde(default)]
    chain: HashMap<String, Value>,
    #[serde(default)]
    federation: HashMap<String, Value>,
    #[serde(default)]
    clients: HashMap<String, Value>,
    #[serde(default)]
    protocol_traffic: HashMap<String, Value>,
    #[serde(default)]
    vms: HashMap<String, Value>,
    #[serde(default)]
    machines: HashMap<String, Value>,
    #[serde(default)]
    costs: HashMap<String, Value>,
    #[serde(default)]
    transactions: HashMap<String, Value>,
    #[serde(default)]
    packets: HashMap<String, Value>,
    #[serde(default)]
    messages: HashMap<String, Value>,
    #[serde(default)]
    creatures: HashMap<String, Value>,
    #[serde(default)]
    validators: HashMap<String, Value>,
    #[serde(default)]
    staking: HashMap<String, Value>,
    #[serde(default)]
    election: HashMap<String, Value>,
    #[serde(default)]
    resources: HashMap<String, Value>,
}

// ───────────────────────── Entry point + dispatch ─────────────────────────

/// Run the canonical CLI implementation. The deprecated `casparctl` binary calls this
/// one-way compatibility edge; canonical code never depends on that alias.
pub fn main() {
    if let Err(error) = aseman_config::install_cli_process_config() {
        eprintln!("invalid Aseman CLI configuration: {error}");
        std::process::exit(2);
    }
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        print_usage();
        std::process::exit(1);
    }
    let result = match args[1].as_str() {
        "install" => {
            // `install --local` is the no-Docker local install phase (verify
            // requirements + generate config); plain `install` is the Docker
            // container setup.
            if args[2..].iter().any(|a| a == "--local") {
                run::install_local(&args[2..])
            } else {
                run_install(&args[2..])
            }
        }
        "uninstall" => run_uninstall(&args[2..]),
        "purge" => run_purge(&args[2..]),
        "start" => run_start(&args[2..]),
        "pause" => run_pause(&args[2..]),
        "resume" => run_resume(&args[2..]),
        "stop" => run_stop(&args[2..]),
        "stats" => run_stats(&args[2..]),
        "run" => run::run_run(&args[2..]),
        "status" => run::run_status(&args[2..]),
        "pprof" => run_pprof(&args[2..]),
        "vms" => vms::run_vms(&args[2..]),
        "cluster" => cluster::run_cluster(&args[2..]),
        "module" | "modules" => modules::run_modules(&args[2..]),
        "doctor" => ops::run_doctor(&args[2..]),
        "backup" => ops::run_backup(&args[2..]),
        "restore" => ops::run_restore(&args[2..]),
        "upgrade" => ops::run_upgrade(&args[2..]),
        "support-bundle" => ops::run_support_bundle(&args[2..]),
        "help" | "-h" | "--help" => {
            print_usage();
            Ok(())
        }
        other => {
            eprintln!("unknown command \"{}\"\n", other);
            print_usage();
            std::process::exit(1);
        }
    };
    if let Err(e) = result {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
}

fn print_usage() {
    println!(
        "asemanctl - administer an Aseman node (legacy command families remain compatible)\n\n\
         Usage:\n  asemanctl <command> [flags]\n\n\
         Commands:\n  \
         install    Full node setup (docker/gvisor/storage/certs/testnet bootstrap)\n  \
         install --local  One-time local install without Docker (requirements + config)\n  \
         uninstall  Stop and remove the Caspar container\n  \
         purge      Uninstall + remove image and volumes\n  \
         start      Start the Caspar container\n  \
         pause      Pause the Caspar container\n  \
         resume     Resume (unpause) the Caspar container\n  \
         stop       Stop the local node (if any), else the Caspar container\n  \
         run        Start the installed local node without Docker (QuestDB + node)\n  \
         status     Show the locally-run node's process/port status\n  \
         stats      Realtime multi-section dashboard (container + telemetry + chain)\n  \
         pprof      Query the node runtime profiler (rust pprof crate)\n  \
         vms        Manage the node's pluggable VM types (list/enable/disable/sync/new)\n  \
         cluster    Orchestrate the geo-distributed instance mesh (peers/config/status)\n\n\
         module     Manage signed provider modules (install/validate/stage/activate/rollback)\n\n\
         doctor     Run the ordered node health checks\n  \
         backup     Snapshot node storage into a signed backup manifest\n  \
         restore    Restore a node from a backup\n  \
         upgrade    Snapshot, stop, replace binaries, migrate, and restart\n  \
         support-bundle  Collect, redact, and package a diagnostics bundle\n\n\
         Run \"asemanctl <command> --help\" for command-specific flags."
    );
}

// ───────────────────────── Tiny flag parser ───────────────────────────────
//
// Go used `flag.NewFlagSet(name, flag.ContinueOnError)`. The Rust equivalent
// (`clap`) requires a per-subcommand struct; the Go behavior is simpler:
// `--key value` or `--key=value`, no positional args. We replicate that here
// so the surface stays identical.

struct FlagSet<'a> {
    name: &'a str,
    strings: HashMap<&'a str, (String, &'a str)>, // key -> (default, help)
    ints: HashMap<&'a str, (i64, &'a str)>,
    durations: HashMap<&'a str, (Duration, &'a str)>,
    parsed_strings: HashMap<&'a str, String>,
    parsed_ints: HashMap<&'a str, i64>,
    parsed_durations: HashMap<&'a str, Duration>,
}

impl<'a> FlagSet<'a> {
    fn new(name: &'a str) -> Self {
        Self {
            name,
            strings: HashMap::new(),
            ints: HashMap::new(),
            durations: HashMap::new(),
            parsed_strings: HashMap::new(),
            parsed_ints: HashMap::new(),
            parsed_durations: HashMap::new(),
        }
    }
    fn string(&mut self, key: &'a str, default: &str, help: &'a str) {
        self.strings.insert(key, (default.to_string(), help));
    }
    fn int(&mut self, key: &'a str, default: i64, help: &'a str) {
        self.ints.insert(key, (default, help));
    }
    fn duration(&mut self, key: &'a str, default: Duration, help: &'a str) {
        self.durations.insert(key, (default, help));
    }
    fn parse(&mut self, args: &[String]) -> Result<()> {
        // Apply defaults.
        for (k, (d, _)) in &self.strings {
            self.parsed_strings.insert(k, d.clone());
        }
        for (k, (d, _)) in &self.ints {
            self.parsed_ints.insert(k, *d);
        }
        for (k, (d, _)) in &self.durations {
            self.parsed_durations.insert(k, *d);
        }
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            if a == "-h" || a == "--help" {
                self.print_help();
                std::process::exit(0);
            }
            let body = if let Some(rest) = a.strip_prefix("--") {
                rest
            } else if let Some(rest) = a.strip_prefix('-') {
                rest
            } else {
                return Err(anyhow!("unexpected positional argument \"{}\"", a));
            };
            let (key_owned, value) = match body.split_once('=') {
                Some((k, v)) => (k.to_string(), Some(v.to_string())),
                None => (body.to_string(), None),
            };
            let key = key_owned.as_str();
            // Resolve key to a registered flag.
            let raw_value = match value {
                Some(v) => v,
                None => {
                    i += 1;
                    if i >= args.len() {
                        return Err(anyhow!("flag \"--{}\" needs a value", key));
                    }
                    args[i].clone()
                }
            };
            if self.strings.contains_key(key) {
                self.parsed_strings.insert(
                    self.strings.keys().find(|k| **k == key).copied().unwrap(),
                    raw_value,
                );
            } else if self.ints.contains_key(key) {
                let v = raw_value
                    .parse::<i64>()
                    .with_context(|| format!("flag \"--{}\" expects int", key))?;
                self.parsed_ints
                    .insert(self.ints.keys().find(|k| **k == key).copied().unwrap(), v);
            } else if self.durations.contains_key(key) {
                let v = parse_duration(&raw_value)
                    .with_context(|| format!("flag \"--{}\" expects duration", key))?;
                self.parsed_durations.insert(
                    self.durations.keys().find(|k| **k == key).copied().unwrap(),
                    v,
                );
            } else {
                return Err(anyhow!("unknown flag \"--{}\"", key));
            }
            i += 1;
        }
        Ok(())
    }
    fn print_help(&self) {
        println!("Usage: casparctl {} [flags]", self.name);
        for (k, (d, h)) in &self.strings {
            println!("  --{:<16} (default \"{}\")  {}", k, d, h);
        }
        for (k, (d, h)) in &self.ints {
            println!("  --{:<16} (default {})        {}", k, d, h);
        }
        for (k, (d, h)) in &self.durations {
            println!("  --{:<16} (default {:?})      {}", k, d, h);
        }
    }
    fn get_string(&self, key: &str) -> String {
        self.parsed_strings.get(key).cloned().unwrap_or_default()
    }
    fn get_int(&self, key: &str) -> i64 {
        self.parsed_ints.get(key).copied().unwrap_or_default()
    }
    fn get_duration(&self, key: &str) -> Duration {
        self.parsed_durations
            .get(key)
            .copied()
            .unwrap_or(Duration::ZERO)
    }
}

fn parse_duration(s: &str) -> Result<Duration> {
    // Mirrors Go's time.ParseDuration for the subset that matters here
    // (ms, s, m, h; fractional allowed; supports plain "2s", "1500ms"…).
    if let Some(num) = s.strip_suffix("ms") {
        let v: f64 = num.parse()?;
        return Ok(Duration::from_micros((v * 1000.0) as u64));
    }
    if let Some(num) = s.strip_suffix('s') {
        let v: f64 = num.parse()?;
        return Ok(Duration::from_micros((v * 1_000_000.0) as u64));
    }
    if let Some(num) = s.strip_suffix('m') {
        let v: f64 = num.parse()?;
        return Ok(Duration::from_secs_f64(v * 60.0));
    }
    if let Some(num) = s.strip_suffix('h') {
        let v: f64 = num.parse()?;
        return Ok(Duration::from_secs_f64(v * 3600.0));
    }
    let v: f64 = s.parse()?;
    Ok(Duration::from_secs_f64(v))
}

// ───────────────────────── install ────────────────────────────────────────

fn run_install(args: &[String]) -> Result<()> {
    let mut fs_set = FlagSet::new("install");
    fs_set.string(
        "project-dir",
        "",
        "path to Caspar node directory (auto-detected when omitted)",
    );
    fs_set.string(
        "env-file",
        ".env",
        "environment file relative to project-dir",
    );
    fs_set.string(
        "envvpath",
        "",
        "path to a ready environment file to copy into project as --env-file",
    );
    fs_set.string("name", "kasper", "docker image name for node image tags");
    fs_set.string(
        "container-name",
        "node1",
        "container name expected by testnet run script",
    );
    fs_set.parse(args)?;

    ensure_docker_ready()?;

    let abs_project = resolve_project_dir(&fs_set.get_string("project-dir"))?;
    let name = fs_set.get_string("name");
    let container_name = fs_set.get_string("container-name");
    validate_docker_name(&name).map_err(|e| anyhow!("invalid --name value: {}", e))?;
    validate_docker_name(&container_name)
        .map_err(|e| anyhow!("invalid --container-name value: {}", e))?;

    let dockerfile = abs_project.join("Dockerfile");
    if !dockerfile.exists() {
        bail!("Dockerfile not found at {}", dockerfile.display());
    }

    let env_rel = fs_set.get_string("env-file");
    let abs_env = abs_project.join(&env_rel);
    let envvpath = fs_set.get_string("envvpath");
    if !envvpath.trim().is_empty() {
        let src_env = fs::canonicalize(envvpath.trim())
            .or_else(|_| Ok::<PathBuf, anyhow::Error>(PathBuf::from(envvpath.trim())))?;
        copy_file(&src_env, &abs_env)
            .with_context(|| format!("failed to copy --envvpath to {}", abs_env.display()))?;
        println!(
            "→ Copied environment file from {} to {}",
            src_env.display(),
            abs_env.display()
        );
    }
    if !abs_env.exists() {
        bail!(
            "env file not found at {} (copy sample.env to .env first)",
            abs_env.display()
        );
    }

    println!("→ Installing and validating gVisor runtime...");
    let scripts_dir = abs_project.join("scripts");
    let install_gvisor_script = scripts_dir.join("install-gvisor.sh");
    if !install_gvisor_script.exists() {
        bail!(
            "required script not found at {}",
            install_gvisor_script.display()
        );
    }
    run_command(Some(&scripts_dir), "bash", &["install-gvisor.sh"])?;
    configure_runsc_runtime()?;

    println!("→ Pulling nginx:alpine image...");
    run_command(None, "docker", &["pull", "nginx:alpine"])?;

    println!("→ Creating storage directories used by the node runtime...");
    ensure_storage_folders()?;

    println!("→ Generating TLS certificate files (cert.pem / cert.key)...");
    ensure_tls_certs("/home/kasper/certs")?;

    println!("→ Building Caspar Docker image (this may take several minutes)...");
    let tag = format!("{}:latest", name);
    let abs_project_str = abs_project.to_string_lossy().to_string();
    let dockerfile_str = dockerfile.to_string_lossy().to_string();
    run_command(
        None,
        "docker",
        &["build", "-t", &tag, "-f", &dockerfile_str, &abs_project_str],
    )?;
    if name != "kasper" {
        run_command(None, "docker", &["tag", &tag, "kasper:latest"])?;
    }

    let _ = run_command_quiet(None, "docker", &["rm", "-f", "kasper-proxy"]);
    let _ = run_command_quiet(None, "docker", &["rm", "-f", &container_name]);

    println!("→ Running prepare-testnet.sh...");
    run_command(Some(&scripts_dir), "bash", &["prepare-testnet.sh"])?;

    println!("→ Running run-testnet.sh...");
    run_command(Some(&scripts_dir), "bash", &["run-testnet.sh"])?;

    write_saved_name(&abs_project, &container_name)?;
    write_saved_image(&abs_project, &name)?;

    let _ = abs_env;
    println!(
        "✓ Caspar testnet node installed and running in container \"{}\"",
        container_name
    );
    println!("  View live dashboard: casparctl stats");
    Ok(())
}

// ───────────────────────── uninstall / purge ──────────────────────────────

fn run_uninstall(args: &[String]) -> Result<()> {
    let mut fs_set = FlagSet::new("uninstall");
    fs_set.string(
        "project-dir",
        "",
        "path to Caspar node directory (auto-detected when omitted)",
    );
    fs_set.parse(args)?;
    require_docker()?;
    let abs_project = resolve_project_dir(&fs_set.get_string("project-dir"))?;
    let name = load_saved_name(&abs_project)?;
    if !container_exists(&name) {
        println!(
            "container \"{}\" does not exist; nothing to uninstall",
            name
        );
        return Ok(());
    }
    run_command(None, "docker", &["rm", "-f", &name])?;
    println!("✓ Uninstalled container \"{}\"", name);
    Ok(())
}

fn run_purge(args: &[String]) -> Result<()> {
    let mut fs_set = FlagSet::new("purge");
    fs_set.string(
        "project-dir",
        "",
        "path to Caspar node directory (auto-detected when omitted)",
    );
    fs_set.parse(args)?;
    require_docker()?;
    let abs_project = resolve_project_dir(&fs_set.get_string("project-dir"))?;
    let name = load_saved_name(&abs_project)?;
    let _ = run_command_quiet(None, "docker", &["rm", "-f", &name]);
    let _ = run_command_quiet(None, "docker", &["rm", "-f", "kasper-proxy"]);
    if let Ok(image_name) = load_saved_image(&abs_project) {
        let tag = format!("{}:latest", image_name);
        let _ = run_command_quiet(None, "docker", &["rmi", &tag]);
    }
    let _ = run_command_quiet(None, "docker", &["rmi", "kasper:latest"]);
    println!("✓ Purged Caspar containers and images for \"{}\"", name);
    Ok(())
}

// ───────────────────────── lifecycle ──────────────────────────────────────

fn run_start(args: &[String]) -> Result<()> {
    lifecycle_command("start", args)
}
fn run_pause(args: &[String]) -> Result<()> {
    lifecycle_command("pause", args)
}
fn run_resume(args: &[String]) -> Result<()> {
    lifecycle_command("unpause", args)
}
fn run_stop(args: &[String]) -> Result<()> {
    // A node started by `casparctl run` is a native process, not a Docker
    // container, so stop it here first. Only fall through to the Docker flow
    // when there is no local node to stop.
    if run::stop_local(args)? {
        return Ok(());
    }
    let mut fs_set = FlagSet::new("stop");
    fs_set.string(
        "project-dir",
        "",
        "path to Caspar node directory (auto-detected when omitted)",
    );
    fs_set.parse(args)?;
    require_docker()?;
    let abs_project = resolve_project_dir(&fs_set.get_string("project-dir"))?;
    let name = load_saved_name(&abs_project)?;
    run_command(None, "docker", &["stop", &name])?;
    println!("✓ Stopped container \"{}\"", name);
    Ok(())
}

fn lifecycle_command(action: &str, args: &[String]) -> Result<()> {
    let mut fs_set = FlagSet::new(action);
    fs_set.string(
        "project-dir",
        "",
        "path to Caspar node directory (auto-detected when omitted)",
    );
    fs_set.parse(args)?;
    require_docker()?;
    let abs_project = resolve_project_dir(&fs_set.get_string("project-dir"))?;
    let name = load_saved_name(&abs_project)?;
    run_command(None, "docker", &[action, &name])?;
    println!(
        "✓ {} completed for container \"{}\"",
        action_label(action),
        name
    );
    Ok(())
}

fn action_label(action: &str) -> &str {
    if action == "unpause" {
        "resume"
    } else {
        action
    }
}

// ───────────────────────── stats dashboard ────────────────────────────────

fn run_stats(args: &[String]) -> Result<()> {
    let mut fs_set = FlagSet::new("stats");
    fs_set.string(
        "project-dir",
        "",
        "path to Caspar node directory (auto-detected when omitted)",
    );
    fs_set.duration("interval", Duration::from_secs(2), "refresh interval");
    fs_set.int("log-lines", 6, "number of recent container logs to show");
    fs_set.parse(args)?;
    require_docker()?;
    let abs_project = resolve_project_dir(&fs_set.get_string("project-dir"))?;
    let name = load_saved_name(&abs_project)?;
    let interval = fs_set.get_duration("interval");
    let log_lines = fs_set.get_int("log-lines").max(0) as usize;

    let stop = Arc::new(AtomicBool::new(false));
    let stop_handler = stop.clone();
    ctrlc::set_handler(move || stop_handler.store(true, Ordering::Relaxed)).ok();

    let mut cpu_history: Vec<f64> = Vec::with_capacity(TREND_MAX_POINTS);

    let render = |cpu_history: &mut Vec<f64>| -> Result<()> {
        let inspect = get_inspect_info(&name).unwrap_or_default();
        let (stats, stats_err) = match get_container_stats(&name) {
            Ok(s) => (Some(s), None),
            Err(e) => (None, Some(e.to_string())),
        };
        let logs = get_container_logs(&name, log_lines).unwrap_or_else(|_| vec![]);
        let (telemetry, telemetry_err) = match get_telemetry_snapshot() {
            Ok(t) => (Some(t), None),
            Err(e) => (None, Some(e.to_string())),
        };
        // Feed the CPU trend from the container stats when running under Docker,
        // otherwise from the node's host CPU in telemetry — so the sparkline
        // works in non-Docker mode too.
        let cpu_point = stats
            .as_ref()
            .and_then(|s| parse_percent(&s.cpu_perc))
            .or_else(|| {
                telemetry
                    .as_ref()
                    .and_then(|t| t.resources.get("cpu_percent"))
                    .and_then(|v| v.as_f64())
            });
        if let Some(cpu) = cpu_point {
            cpu_history.push(cpu);
            if cpu_history.len() > TREND_MAX_POINTS {
                let cut = cpu_history.len() - TREND_MAX_POINTS;
                cpu_history.drain(..cut);
            }
        }
        render_dashboard(
            &inspect,
            stats.as_ref(),
            stats_err.as_deref(),
            &logs,
            telemetry.as_ref(),
            telemetry_err.as_deref(),
            interval,
            cpu_history,
        );
        Ok(())
    };

    render(&mut cpu_history)?;
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(interval);
        if stop.load(Ordering::Relaxed) {
            break;
        }
        render(&mut cpu_history)?;
    }
    println!("\nExiting Caspar dashboard.");
    Ok(())
}

fn get_inspect_info(container: &str) -> Result<InspectInfo> {
    let out = Command::new("docker")
        .args(["inspect", container])
        .output()?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(anyhow!(
            "docker inspect failed: {}",
            if msg.is_empty() {
                "non-zero exit".to_string()
            } else {
                msg
            }
        ));
    }
    let raw: Value = serde_json::from_slice(&out.stdout)
        .with_context(|| "cannot parse docker inspect output")?;
    let arr = raw.as_array().ok_or_else(|| anyhow!("not an array"))?;
    if arr.is_empty() {
        return Err(anyhow!("docker inspect returned no container data"));
    }
    let item = &arr[0];
    let mut info = InspectInfo {
        name: item
            .pointer("/Name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim_start_matches('/')
            .to_string(),
        image: item
            .pointer("/Config/Image")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        state: item
            .pointer("/State/Status")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        created_at: short_time(
            item.pointer("/Created")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        ),
        started_at: short_time(
            item.pointer("/State/StartedAt")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        ),
        restart_count: item
            .pointer("/RestartCount")
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        health: "n/a".to_string(),
        ports: vec![],
        mounts: vec![],
    };
    if let Some(h) = item
        .pointer("/State/Health/Status")
        .and_then(|v| v.as_str())
        && !h.is_empty()
    {
        info.health = h.to_string();
    }
    if let Some(pb) = item
        .pointer("/HostConfig/PortBindings")
        .and_then(|v| v.as_object())
    {
        for (p, binds) in pb {
            let arr = binds.as_array();
            if arr.is_none_or(|a| a.is_empty()) {
                info.ports.push(format!("{} (internal)", p));
                continue;
            }
            for b in arr.unwrap() {
                let host_ip = b.get("HostIp").and_then(|v| v.as_str()).unwrap_or("");
                let host_port = b.get("HostPort").and_then(|v| v.as_str()).unwrap_or("");
                let host = if host_ip.is_empty() {
                    "0.0.0.0"
                } else {
                    host_ip
                };
                info.ports.push(format!("{}:{} -> {}", host, host_port, p));
            }
        }
    }
    if info.ports.is_empty() {
        info.ports = vec!["none".to_string()];
    }
    if let Some(mounts) = item.pointer("/Mounts").and_then(|v| v.as_array()) {
        for m in mounts {
            let src = m.get("Source").and_then(|v| v.as_str()).unwrap_or("");
            let dst = m.get("Destination").and_then(|v| v.as_str()).unwrap_or("");
            info.mounts.push(format!("{} -> {}", src, dst));
        }
    }
    if info.mounts.is_empty() {
        info.mounts = vec!["none".to_string()];
    }
    Ok(info)
}

fn get_container_stats(container: &str) -> Result<DockerStats> {
    let out = Command::new("docker")
        .args(["stats", "--no-stream", "--format", "{{json .}}", container])
        .output()?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(anyhow!(
            "docker stats failed: {}",
            if msg.is_empty() {
                "non-zero exit".into()
            } else {
                msg
            }
        ));
    }
    let s = String::from_utf8_lossy(&out.stdout);
    for line in s.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let stats: DockerStats =
            serde_json::from_str(line).with_context(|| "cannot parse docker stats output")?;
        return Ok(stats);
    }
    Err(anyhow!(
        "no stats returned (container may be stopped/paused)"
    ))
}

fn get_container_logs(container: &str, lines: usize) -> Result<Vec<String>> {
    let out = Command::new("docker")
        .args(["logs", "--tail", &lines.to_string(), container])
        .output()?;
    if !out.status.success() && out.stdout.is_empty() {
        return Err(anyhow!("docker logs failed"));
    }
    let mut res: Vec<String> = Vec::new();
    for s in [&out.stdout, &out.stderr] {
        let txt = String::from_utf8_lossy(s);
        for line in txt.lines() {
            let line = line.trim();
            if !line.is_empty() {
                res.push(line.to_string());
            }
        }
    }
    if res.is_empty() {
        res.push("(no recent logs)".to_string());
    }
    Ok(res)
}

#[allow(clippy::too_many_arguments)] // Compatibility dashboard mirrors the legacy sections.
fn render_dashboard(
    info: &InspectInfo,
    stats: Option<&DockerStats>,
    stats_err: Option<&str>,
    logs: &[String],
    telemetry: Option<&TelemetrySnapshot>,
    telemetry_err: Option<&str>,
    interval: Duration,
    cpu_history: &[f64],
) {
    clear_screen();
    let now = Local::now().format("%a, %d %b %Y %H:%M:%S %Z").to_string();
    print_section(
        "CASPAR NODE DASHBOARD",
        &[
            format!("Updated: {}", now),
            format!("Refresh: {}", format_duration(interval)),
            format!("Container: {}", info.name),
            format!(
                "State: {}   Health: {}",
                info.state.to_uppercase(),
                info.health.to_uppercase()
            ),
        ],
    );
    print_section(
        "RUNTIME OVERVIEW",
        &[
            format!("Image: {}", info.image),
            format!("Created: {}", info.created_at),
            format!("Started: {}", info.started_at),
            format!("Restart count: {}", info.restart_count),
        ],
    );
    let trend = render_sparkline(cpu_history);
    if let Some(err) = stats_err {
        // Non-Docker mode (bare `casparctl run`): container-level stats are not
        // available, but the node reports host CPU / RAM / disk in its telemetry
        // snapshot, so resource usage is shown below in HOST RESOURCES anyway.
        print_section(
            "RESOURCE STATS (CONTAINER)",
            &[
                format!("Container stats unavailable: {}", err),
                "Node is not running under Docker — see HOST RESOURCES below.".to_string(),
            ],
        );
    } else if let Some(s) = stats {
        print_section(
            "RESOURCE STATS (CONTAINER)",
            &[
                format!(
                    "CPU: {}   MEM: {} ({})",
                    s.cpu_perc, s.mem_usage, s.mem_perc
                ),
                format!("NET I/O: {}", s.net_io),
                format!("BLOCK I/O: {}", s.block_io),
                format!("PIDs: {}", s.pids),
            ],
        );
    }
    // HOST RESOURCES — the node's own machine CPU / RAM / disk, from telemetry.
    // Works in Docker and non-Docker mode alike (point in the task brief), and
    // carries the CPU sparkline so a trend is always shown.
    print_section("HOST RESOURCES", &host_resource_lines(telemetry, &trend));
    print_section("PORT MAPPINGS", &info.ports);
    if let Some(err) = telemetry_err {
        print_section(
            "CASPAR TELEMETRY",
            &[format!("Telemetry unavailable: {}", err)],
        );
    } else {
        print_section("CASPAR TELEMETRY", &telemetry_lines(telemetry));
        // Chain stats — point (1) of the task brief. The chain service
        // module data lives inside the telemetry snapshot's `chain` field;
        // we surface it explicitly so it's not buried in the generic
        // "Chain: {…}" line.
        print_section("CHAIN STATS", &chain_stats_lines(telemetry));
    }
    print_section("MOUNTS", &info.mounts);
    print_section("RECENT LOGS", &pad_logs(logs, 8));
    print_section(
        "ACTIONS",
        &[
            "Start:     casparctl start".to_string(),
            "Pause:     casparctl pause".to_string(),
            "Resume:    casparctl resume".to_string(),
            "Stop:      casparctl stop".to_string(),
            "Cleanup:   casparctl purge".to_string(),
            "Profiler:  casparctl pprof runtime / heap / flamegraph".to_string(),
        ],
    );
    println!("Press Ctrl+C to exit dashboard");
}

fn get_telemetry_snapshot() -> Result<TelemetrySnapshot> {
    let url = aseman_config::cli_config()
        .map(|config| config.telemetry_url.as_str())
        .unwrap_or("http://127.0.0.1:9099/telemetry/snapshot");
    let body = curl_get(url, Duration::from_millis(1200))?;
    Ok(serde_json::from_slice(&body)?)
}

fn telemetry_lines(t: Option<&TelemetrySnapshot>) -> Vec<String> {
    let Some(t) = t else {
        return vec!["(no telemetry)".to_string()];
    };
    vec![
        format!("Uptime: {}s   Snapshot: {}", t.uptime_sec, t.timestamp),
        format!("Node: {}", compact_map(&t.node)),
        format!("Federation: {}", compact_map(&t.federation)),
        format!("Clients: {}", compact_map(&t.clients)),
        format!("Protocol traffic: {}", compact_map(&t.protocol_traffic)),
        format!("VMs: {}", compact_map(&t.vms)),
        format!("Machines: {}", compact_map(&t.machines)),
        format!("Costs: {}", compact_map(&t.costs)),
        format!("Transactions: {}", compact_map(&t.transactions)),
        format!("Packets: {}", compact_map(&t.packets)),
        format!("Messages: {}", compact_map(&t.messages)),
        format!("Creatures: {}", compact_map(&t.creatures)),
    ]
}

fn chain_stats_lines(t: Option<&TelemetrySnapshot>) -> Vec<String> {
    // Renders telemetry's `chain` / `validators` / `staking` / `election`
    // fields — the same data the Babble chain `service.go` would have
    // returned from `/stats`, `/peers`, `/validators`, `/history`. The
    // node's telemetry collector already proxies the live chain endpoints
    // into these fields (see `apps/aseman-node/src/telemetry/server.rs::collect`).
    let Some(t) = t else {
        return vec!["(no chain data)".to_string()];
    };
    let mut lines = Vec::new();
    if let Some(stats) = t.chain.get("stats") {
        // /stats — map[string]string of node internal state.
        if let Some(obj) = stats.as_object() {
            if obj.is_empty() {
                lines.push("Stats: (node not exposing /stats yet)".to_string());
            } else {
                for (k, v) in obj {
                    lines.push(format!("  {} = {}", k, render_compact_value(v)));
                }
            }
        } else if !stats.is_null() {
            lines.push(format!("Stats: {}", render_compact_value(stats)));
        }
    }
    if let Some(peers) = t.chain.get("peers") {
        // /peers — array of peers.Peer{PubKeyHex, Moniker, NetAddr}.
        if let Some(arr) = peers.as_array() {
            lines.push(format!("Peers: {}", arr.len()));
            for p in arr.iter().take(5) {
                let moniker = p.get("Moniker").and_then(|v| v.as_str()).unwrap_or("?");
                let addr = p.get("NetAddr").and_then(|v| v.as_str()).unwrap_or("?");
                lines.push(format!("  ↳ {} @ {}", moniker, addr));
            }
            if arr.len() > 5 {
                lines.push(format!("  …(+{} more)", arr.len() - 5));
            }
        }
    }
    lines.push(format!("Validators: {}", compact_map(&t.validators)));
    lines.push(format!("Staking: {}", compact_map(&t.staking)));
    lines.push(format!("Election: {}", compact_map(&t.election)));
    if lines.is_empty() {
        lines.push("(no chain data)".to_string());
    }
    lines
}

/// Host CPU / memory / disk lines from the telemetry snapshot's `resources`
/// map (collected by the node from `/proc` + `statvfs`, so it is present in both
/// Docker and bare-process runs). Renders a placeholder when the running node is
/// too old to report resources yet.
fn host_resource_lines(t: Option<&TelemetrySnapshot>, cpu_trend: &str) -> Vec<String> {
    let Some(t) = t else {
        return vec!["(no telemetry)".to_string()];
    };
    let r = &t.resources;
    if r.is_empty() {
        return vec!["(node not reporting host resources — telemetry too old)".to_string()];
    }
    let num = |k: &str| -> Option<f64> { r.get(k).and_then(|v| v.as_f64()) };
    let mut lines = Vec::new();

    let cpu = num("cpu_percent");
    let cores = num("cpu_cores");
    lines.push(format!(
        "CPU: {}   Cores: {}   Trend: {}",
        cpu.map(|v| format!("{:.1}%", v))
            .unwrap_or_else(|| "n/a".into()),
        cores
            .map(|v| format!("{}", v as u64))
            .unwrap_or_else(|| "?".into()),
        cpu_trend,
    ));
    if let (Some(l1), Some(l5), Some(l15)) =
        (num("load_avg_1m"), num("load_avg_5m"), num("load_avg_15m"))
    {
        lines.push(format!(
            "Load avg: {:.2}  {:.2}  {:.2}  (1m/5m/15m)",
            l1, l5, l15
        ));
    }
    if let (Some(used), Some(total)) = (num("mem_used_bytes"), num("mem_total_bytes")) {
        lines.push(format!(
            "MEM: {} / {} ({})",
            human_bytes(used),
            human_bytes(total),
            num("mem_used_percent")
                .map(|v| format!("{:.1}%", v))
                .unwrap_or_else(|| "?".into()),
        ));
    }
    if let (Some(used), Some(total)) = (num("disk_used_bytes"), num("disk_total_bytes")) {
        let path = r.get("disk_path").and_then(|v| v.as_str()).unwrap_or("/");
        lines.push(format!(
            "DISK ({}): {} / {} ({})",
            path,
            human_bytes(used),
            human_bytes(total),
            num("disk_used_percent")
                .map(|v| format!("{:.1}%", v))
                .unwrap_or_else(|| "?".into()),
        ));
    }
    if let Some(rss) = num("process_rss_bytes") {
        lines.push(format!("Node process RSS: {}", human_bytes(rss)));
    }
    if let Some(up) = num("host_uptime_sec") {
        lines.push(format!(
            "Host uptime: {}",
            format_duration(Duration::from_secs(up as u64))
        ));
    }
    lines
}

/// Human-readable byte size (base-1024) for the resource lines.
fn human_bytes(v: f64) -> String {
    if !v.is_finite() || v < 0.0 {
        return "n/a".to_string();
    }
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut val = v;
    let mut idx = 0;
    while val >= 1024.0 && idx < UNITS.len() - 1 {
        val /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{} {}", val as u64, UNITS[idx])
    } else {
        format!("{:.2} {}", val, UNITS[idx])
    }
}

fn render_compact_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        _ => serde_json::to_string(v).unwrap_or_else(|_| "{…}".to_string()),
    }
}

fn compact_map(m: &HashMap<String, Value>) -> String {
    if m.is_empty() {
        return "{}".to_string();
    }
    match serde_json::to_string(m) {
        Ok(s) => s,
        Err(_) => "{...}".to_string(),
    }
}

fn print_section(title: &str, lines: &[String]) {
    let width = DASHBOARD_WIDTH;
    println!("┌{}┐", "─".repeat(width - 2));
    println!("{}", box_line(width, &format!("◆ {}", title)));
    println!("├{}┤", "─".repeat(width - 2));
    let render: Vec<&str> = if lines.is_empty() {
        vec!["(empty)"]
    } else {
        lines.iter().map(|s| s.as_str()).collect()
    };
    for line in render {
        println!("{}", box_line(width, line));
    }
    println!("└{}┘", "─".repeat(width - 2));
}

fn pad_logs(logs: &[String], n: usize) -> Vec<String> {
    if logs.len() >= n {
        return logs[logs.len() - n..].to_vec();
    }
    let mut out = Vec::with_capacity(n);
    for _ in 0..(n - logs.len()) {
        out.push(String::new());
    }
    out.extend(logs.iter().cloned());
    out
}

fn box_line(width: usize, content: &str) -> String {
    if width < 2 {
        return content.to_string();
    }
    let max = width - 4;
    let trimmed = if rune_count(content) > max {
        trim_runes(content, max)
    } else {
        content.to_string()
    };
    let pad = max - rune_count(&trimmed);
    format!("│ {}{} │", trimmed, " ".repeat(pad))
}

fn rune_count(s: &str) -> usize {
    s.chars().count()
}

fn trim_runes(s: &str, n: usize) -> String {
    let runes: Vec<char> = s.chars().collect();
    if runes.len() <= n {
        return s.to_string();
    }
    if n <= 1 {
        return runes[..n].iter().collect();
    }
    let mut out: String = runes[..n - 1].iter().collect();
    out.push('…');
    out
}

fn render_sparkline(values: &[f64]) -> String {
    if values.is_empty() {
        return "(warming up)".to_string();
    }
    let bars: Vec<char> = "▁▂▃▄▅▆▇█".chars().collect();
    let max = values.iter().cloned().fold(0.0_f64, f64::max);
    if max <= 0.0 {
        return "▁".repeat(values.len());
    }
    let mut out = String::new();
    for v in values {
        let mut idx = ((v / max) * (bars.len() as f64 - 1.0)) as isize;
        if idx < 0 {
            idx = 0;
        }
        if idx as usize >= bars.len() {
            idx = bars.len() as isize - 1;
        }
        out.push(bars[idx as usize]);
    }
    out
}

fn parse_percent(p: &str) -> Option<f64> {
    let v = p.trim().trim_end_matches('%');
    if v.is_empty() {
        return None;
    }
    v.parse::<f64>().ok()
}

fn short_time(raw: &str) -> String {
    if raw.is_empty() || raw == "0001-01-01T00:00:00Z" {
        return "n/a".to_string();
    }
    match DateTime::parse_from_rfc3339(raw) {
        Ok(t) => t
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M:%S %Z")
            .to_string(),
        Err(_) => raw.to_string(),
    }
}

fn format_duration(d: Duration) -> String {
    if d >= Duration::from_secs(1) {
        format!("{:.1}s", d.as_secs_f64())
    } else {
        format!("{}ms", d.as_millis())
    }
}

fn clear_screen() {
    if cfg!(target_os = "windows") {
        let _ = run_command_quiet(None, "cmd", &["/c", "cls"]);
    } else {
        print!("\x1b[H\x1b[2J");
        let _ = std::io::stdout().flush();
    }
}

// ───────────────────────── pprof subcommand ───────────────────────────────

fn run_pprof(args: &[String]) -> Result<()> {
    if args.is_empty() {
        print_pprof_usage();
        return Ok(());
    }
    let sub = &args[0];
    let rest = &args[1..];
    let name = format!("pprof {}", sub);
    let host_default = default_pprof_host();
    let mut fs_set = FlagSet::new(&name);
    fs_set.string("host", &host_default, "node profiler base URL");
    fs_set.int("seconds", 5, "sample window for cpu profiling (1..=60)");
    fs_set.string(
        "output",
        "",
        "write the response body to this file (default stdout)",
    );
    fs_set.parse(rest)?;
    let host = fs_set.get_string("host");
    let seconds = fs_set.get_int("seconds").clamp(1, 60);
    let output = fs_set.get_string("output");

    let (path, suggested_ext) = match sub.as_str() {
        "runtime" => ("/debug/pprof/runtime".to_string(), ".json"),
        "heap" => ("/debug/pprof/heap".to_string(), ".json"),
        "threads" => ("/debug/pprof/threads".to_string(), ".json"),
        "flamegraph" => (
            format!("/debug/pprof/flamegraph?seconds={}", seconds),
            ".svg",
        ),
        "profile" => (format!("/debug/pprof/profile?seconds={}", seconds), ".pb"),
        "help" | "-h" | "--help" => {
            print_pprof_usage();
            return Ok(());
        }
        other => {
            print_pprof_usage();
            return Err(anyhow!("unknown pprof subcommand \"{}\"", other));
        }
    };

    let url = format!("{}{}", host.trim_end_matches('/'), path);
    let timeout = Duration::from_secs(seconds as u64 + 10);
    if matches!(sub.as_str(), "flamegraph" | "profile") {
        println!("→ sampling {}s on {} …", seconds, host);
    }
    let body = curl_get(&url, timeout)
        .with_context(|| format!("query {} (is the node running with PPROF_PORT?)", url))?;

    if output.trim().is_empty() {
        match sub.as_str() {
            "flamegraph" | "profile" => {
                let default_name = format!("casparctl-pprof-{}{}", sub, suggested_ext);
                let path = PathBuf::from(&default_name);
                fs::write(&path, &body)?;
                println!("✓ wrote {} ({} bytes)", path.display(), body.len());
                if sub == "flamegraph" {
                    println!("  open it in a browser to inspect the CPU flamegraph");
                } else {
                    println!("  inspect with: go tool pprof {}", path.display());
                }
            }
            _ => {
                // JSON — pretty-print.
                match serde_json::from_slice::<Value>(&body) {
                    Ok(v) => println!("{}", serde_json::to_string_pretty(&v)?),
                    Err(_) => std::io::stdout().write_all(&body)?,
                }
            }
        }
    } else {
        fs::write(&output, &body)?;
        println!("✓ wrote {} ({} bytes)", output, body.len());
    }
    Ok(())
}

fn default_pprof_host() -> String {
    aseman_config::cli_config()
        .map(|config| config.pprof_url.clone())
        .unwrap_or_else(|| "http://127.0.0.1:9999".to_string())
}

fn print_pprof_usage() {
    println!(
        "casparctl pprof <subcommand> [flags]\n\n\
         Subcommands:\n  \
         runtime     show pid / uptime / thread count / os / arch (JSON)\n  \
         heap        show per-process memory counters from /proc (JSON)\n  \
         threads     list all OS threads owned by the node (JSON)\n  \
         flamegraph  sample CPU and render an SVG flamegraph (--seconds N)\n  \
         profile     sample CPU and dump a pprof protobuf (--seconds N)\n\n\
         Flags:\n  \
         --host URL       profiler base URL (default {})\n  \
         --seconds N      sample window 1..=60 (default 5)\n  \
         --output FILE    save the body here instead of stdout/auto-name\n\n\
         The node side is the Rust `pprof` crate; the SVG opens in any browser\n\
         and the .pb is loadable with `go tool pprof <file>`.",
        default_pprof_host()
    );
}

// ───────────────────────── helpers: docker / fs / shell ───────────────────

fn require_docker() -> Result<()> {
    if which::which("docker").is_err() && Command::new("docker").arg("--version").output().is_err()
    {
        bail!("docker is required but was not found in PATH");
    }
    let status = Command::new("docker")
        .arg("info")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        _ => bail!("docker daemon is not reachable; start Docker first"),
    }
}

mod which {
    use std::path::PathBuf;
    pub fn which(prog: &str) -> Result<PathBuf, ()> {
        let path = aseman_config::cli_config()
            .and_then(|config| config.executable_path.as_ref())
            .ok_or(())?;
        for dir in std::env::split_paths(path) {
            let candidate = dir.join(prog);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        Err(())
    }
}

fn ensure_docker_ready() -> Result<()> {
    if require_docker().is_ok() {
        return Ok(());
    }
    if !cfg!(target_os = "linux") {
        bail!("docker is required and auto-install is only implemented on linux");
    }
    println!("→ Docker was not detected; attempting automatic installation...");
    run_privileged_command("apt-get", &["update"])
        .context("docker install failed during apt-get update")?;
    run_privileged_command("apt-get", &["install", "-y", "docker.io"])
        .context("docker install failed during apt-get install docker.io")?;
    let _ = run_privileged_command("systemctl", &["enable", "--now", "docker"]);
    require_docker()
}

fn run_privileged_command(name: &str, args: &[&str]) -> Result<()> {
    if run_command(None, name, args).is_ok() {
        return Ok(());
    }
    if which::which("sudo").is_ok() {
        let mut full: Vec<&str> = Vec::with_capacity(args.len() + 1);
        full.push(name);
        full.extend_from_slice(args);
        return run_command(None, "sudo", &full);
    }
    run_command(None, name, args)
}

fn configure_runsc_runtime() -> Result<()> {
    let daemon_path = "/etc/docker/daemon.json";
    let mut cfg: Value = if let Ok(raw) = fs::read_to_string(daemon_path) {
        if raw.trim().is_empty() {
            Value::Object(Default::default())
        } else {
            serde_json::from_str(&raw)
                .with_context(|| format!("failed to parse {}", daemon_path))?
        }
    } else {
        Value::Object(Default::default())
    };
    let obj = cfg
        .as_object_mut()
        .ok_or_else(|| anyhow!("{} is not a JSON object", daemon_path))?;
    let runtimes = obj
        .entry("runtimes".to_string())
        .or_insert_with(|| Value::Object(Default::default()));
    let runtimes_obj = runtimes
        .as_object_mut()
        .ok_or_else(|| anyhow!("runtimes is not an object"))?;
    runtimes_obj.insert(
        "runsc".to_string(),
        serde_json::json!({
            "path": "runsc",
            "runtimeArgs": ["--network=host"],
        }),
    );
    let pretty = serde_json::to_vec_pretty(&cfg)?;
    let tmp = std::env::temp_dir().join(format!("casparctl-daemon-{}.json", std::process::id()));
    fs::write(&tmp, pretty)?;
    let tmp_str = tmp.to_string_lossy().to_string();
    let res = run_privileged_command("cp", &[&tmp_str, daemon_path]);
    let _ = fs::remove_file(&tmp);
    res.with_context(|| format!("failed to write {}", daemon_path))?;
    let _ = run_privileged_command("systemctl", &["restart", "docker"]);
    Ok(())
}

fn ensure_storage_folders() -> Result<()> {
    let dirs = [
        "/home/kasper/data",
        "/home/kasper/data/docker_proxy",
        "/home/kasper/data/docker_proxy/ssl",
        "/home/kasper/data/files",
        "/home/kasper/data/keys",
        "/home/kasper/data/chains",
        "/home/kasper/data/vm_stores",
        "/home/kasper/data/db",
        "/home/kasper/data/db/base",
        "/home/kasper/data/db/applet",
        "/home/kasper/certs",
        "/home/kasper/packets",
        "/root/.babble",
    ];
    for d in dirs {
        run_privileged_command("mkdir", &["-p", d])?;
    }
    Ok(())
}

fn ensure_tls_certs(cert_dir: &str) -> Result<()> {
    let cert_path = PathBuf::from(cert_dir).join("cert.pem");
    let key_path = PathBuf::from(cert_dir).join("cert.key");
    if cert_path.exists() && key_path.exists() {
        return Ok(());
    }
    let cert_str = cert_path.to_string_lossy().to_string();
    let key_str = key_path.to_string_lossy().to_string();
    run_command(
        None,
        "openssl",
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "3650",
            "-keyout",
            &key_str,
            "-out",
            &cert_str,
            "-subj",
            "/CN=caspar.local",
            "-addext",
            "subjectAltName=DNS:localhost,DNS:*.localhost,IP:127.0.0.1",
        ],
    )?;
    let fullchain = PathBuf::from(cert_dir).join("fullchain.pem");
    let privkey = PathBuf::from(cert_dir).join("privkey.pem");
    let _ = run_command_quiet(None, "cp", &[&cert_str, &fullchain.to_string_lossy()]);
    let _ = run_command_quiet(None, "cp", &[&key_str, &privkey.to_string_lossy()]);
    Ok(())
}

fn validate_docker_name(name: &str) -> Result<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        bail!("name must not be empty");
    }
    if !docker_name_re().is_match(trimmed) {
        bail!("allowed characters are letters, numbers, dot, underscore, and hyphen");
    }
    Ok(())
}

fn name_file_path(project_dir: &Path) -> Result<PathBuf> {
    Ok(fs::canonicalize(project_dir)
        .unwrap_or_else(|_| project_dir.to_path_buf())
        .join(NAME_FILE_NAME))
}

fn image_file_path(project_dir: &Path) -> Result<PathBuf> {
    Ok(fs::canonicalize(project_dir)
        .unwrap_or_else(|_| project_dir.to_path_buf())
        .join(IMAGE_FILE_NAME))
}

fn resolve_project_dir(project_dir: &str) -> Result<PathBuf> {
    if !project_dir.trim().is_empty() {
        let abs =
            fs::canonicalize(project_dir).unwrap_or_else(|_| PathBuf::from(project_dir.trim()));
        validate_node_project_dir(&abs)?;
        return Ok(abs);
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let exe = std::env::current_exe().unwrap_or_default();
    let exe_dir = exe.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let candidates = [
        cwd.join("node"),
        cwd.join("..").join("node"),
        cwd.join("..").join("..").join("node"),
        exe_dir.join("node"),
        exe_dir.join("..").join("node"),
        exe_dir.join("..").join("..").join("node"),
    ];
    for c in &candidates {
        let abs = fs::canonicalize(c).unwrap_or_else(|_| c.clone());
        if validate_node_project_dir(&abs).is_ok() {
            return Ok(abs);
        }
    }
    bail!("could not auto-detect node project directory; pass --project-dir explicitly")
}

fn validate_node_project_dir(dir: &Path) -> Result<()> {
    let dockerfile = dir.join("Dockerfile");
    if !dockerfile.exists() {
        bail!("node Dockerfile not found in {}", dir.display());
    }
    Ok(())
}

fn write_saved_name(project_dir: &Path, name: &str) -> Result<()> {
    validate_docker_name(name)?;
    let path = name_file_path(project_dir)?;
    fs::write(&path, format!("{}\n", name.trim()))?;
    Ok(())
}

fn write_saved_image(project_dir: &Path, image: &str) -> Result<()> {
    validate_docker_name(image)?;
    let path = image_file_path(project_dir)?;
    fs::write(&path, format!("{}\n", image.trim()))?;
    Ok(())
}

fn load_saved_name(project_dir: &Path) -> Result<String> {
    let path = name_file_path(project_dir)?;
    let data = fs::read_to_string(&path).with_context(|| {
        format!(
            "could not read {}; run install first to generate it",
            path.display()
        )
    })?;
    let name = data.trim().to_string();
    validate_docker_name(&name)
        .with_context(|| format!("invalid name stored in {}", path.display()))?;
    Ok(name)
}

fn load_saved_image(project_dir: &Path) -> Result<String> {
    let path = image_file_path(project_dir)?;
    let data = fs::read_to_string(&path).with_context(|| {
        format!(
            "could not read {}; run install first to generate it",
            path.display()
        )
    })?;
    let name = data.trim().to_string();
    validate_docker_name(&name)
        .with_context(|| format!("invalid image name stored in {}", path.display()))?;
    Ok(name)
}

fn container_exists(container: &str) -> bool {
    Command::new("docker")
        .args(["container", "inspect", container])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn copy_file(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(src, dst)?;
    Ok(())
}

fn run_command(workdir: Option<&Path>, name: &str, args: &[&str]) -> Result<()> {
    let mut cmd = Command::new(name);
    cmd.args(args);
    if let Some(d) = workdir {
        cmd.current_dir(d);
    }
    let status = cmd.status()?;
    if !status.success() {
        bail!("`{} {:?}` failed (exit {:?})", name, args, status.code());
    }
    Ok(())
}

fn run_command_quiet(workdir: Option<&Path>, name: &str, args: &[&str]) -> Result<()> {
    let mut cmd = Command::new(name);
    cmd.args(args);
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    if let Some(d) = workdir {
        cmd.current_dir(d);
    }
    let status = cmd.status()?;
    if !status.success() {
        bail!("`{} {:?}` failed (exit {:?})", name, args, status.code());
    }
    Ok(())
}

fn curl_get(url: &str, timeout: Duration) -> Result<Vec<u8>> {
    let timeout_str = timeout.as_secs().max(1).to_string();
    let start = Instant::now();
    let out = Command::new("curl")
        .args(["-sS", "--fail-with-body", "--max-time", &timeout_str, url])
        .output()?;
    let _elapsed = start.elapsed();
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(anyhow!(
            "curl {} -> {}",
            url,
            if msg.is_empty() {
                "non-zero exit".to_string()
            } else {
                msg
            }
        ));
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── parse_duration ──────────────────────────────────────────────────

    #[test]
    fn parse_duration_milliseconds() {
        let d = parse_duration("250ms").unwrap();
        assert_eq!(d, Duration::from_micros(250_000));
    }

    #[test]
    fn parse_duration_seconds() {
        let d = parse_duration("2s").unwrap();
        assert_eq!(d, Duration::from_micros(2_000_000));
    }

    #[test]
    fn parse_duration_fractional_seconds() {
        let d = parse_duration("0.5s").unwrap();
        assert_eq!(d, Duration::from_micros(500_000));
    }

    #[test]
    fn parse_duration_minutes_and_hours() {
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
    }

    #[test]
    fn parse_duration_bare_number_is_seconds() {
        let d = parse_duration("3").unwrap();
        assert_eq!(d, Duration::from_secs(3));
    }

    #[test]
    fn parse_duration_rejects_garbage() {
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("12x").is_err());
    }

    // ─── parse_percent ──────────────────────────────────────────────────

    #[test]
    fn parse_percent_handles_trailing_pct() {
        assert_eq!(parse_percent("42.5%"), Some(42.5));
        // Outer whitespace is trimmed before parsing.
        assert_eq!(parse_percent(" 17%"), Some(17.0));
        assert_eq!(parse_percent("100%"), Some(100.0));
        // No percent sign is fine; the function just parses the number.
        assert_eq!(parse_percent("3.5"), Some(3.5));
    }

    #[test]
    fn parse_percent_rejects_empty_and_garbage() {
        assert_eq!(parse_percent(""), None);
        assert_eq!(parse_percent("%"), None);
        assert_eq!(parse_percent("not a number"), None);
    }

    // ─── format_duration ─────────────────────────────────────────────────

    #[test]
    fn format_duration_uses_seconds_for_one_second_and_above() {
        assert_eq!(format_duration(Duration::from_secs(1)), "1.0s");
        assert_eq!(format_duration(Duration::from_millis(2500)), "2.5s");
    }

    #[test]
    fn format_duration_uses_milliseconds_below_one_second() {
        assert_eq!(format_duration(Duration::from_millis(500)), "500ms");
        assert_eq!(format_duration(Duration::from_millis(0)), "0ms");
    }

    // ─── rune_count / trim_runes ─────────────────────────────────────────

    #[test]
    fn rune_count_counts_unicode_codepoints() {
        assert_eq!(rune_count(""), 0);
        assert_eq!(rune_count("abc"), 3);
        // Each multibyte char counts as 1 rune.
        assert_eq!(rune_count("héllo"), 5);
        assert_eq!(rune_count("日本語"), 3);
    }

    #[test]
    fn trim_runes_leaves_short_strings_alone() {
        assert_eq!(trim_runes("abc", 5), "abc");
    }

    #[test]
    fn trim_runes_truncates_and_adds_ellipsis() {
        // n > 1 -> last char is replaced by '…'.
        assert_eq!(trim_runes("abcdef", 4), "abc…");
    }

    #[test]
    fn trim_runes_at_one_returns_single_char_without_ellipsis() {
        let s = trim_runes("abc", 1);
        assert_eq!(s.chars().count(), 1);
    }

    // ─── render_sparkline ────────────────────────────────────────────────

    #[test]
    fn render_sparkline_warms_up_on_empty() {
        assert_eq!(render_sparkline(&[]), "(warming up)");
    }

    #[test]
    fn render_sparkline_handles_all_zero_values() {
        // All-zero input yields the lowest bar repeated.
        let s = render_sparkline(&[0.0, 0.0, 0.0]);
        assert_eq!(s.chars().count(), 3);
        assert!(s.chars().all(|c| c == '▁'));
    }

    #[test]
    fn render_sparkline_uses_full_bar_for_max_value() {
        let s = render_sparkline(&[1.0, 5.0, 10.0]);
        assert_eq!(s.chars().count(), 3);
        // The last char is the maximum bar.
        assert_eq!(s.chars().last(), Some('█'));
    }

    // ─── validate_docker_name ────────────────────────────────────────────

    #[test]
    fn validate_docker_name_accepts_typical_names() {
        validate_docker_name("caspar-node").unwrap();
        validate_docker_name("caspar_node-1").unwrap();
        validate_docker_name("a.b.c").unwrap();
        validate_docker_name("ABC123").unwrap();
    }

    #[test]
    fn validate_docker_name_rejects_empty() {
        let err = validate_docker_name("   ").unwrap_err();
        assert!(format!("{}", err).contains("not be empty"));
    }

    #[test]
    fn validate_docker_name_rejects_special_characters() {
        assert!(validate_docker_name("bad name").is_err());
        assert!(validate_docker_name("bad/name").is_err());
        assert!(validate_docker_name("bad:name").is_err());
        assert!(validate_docker_name("bad$name").is_err());
    }

    // ─── short_time ──────────────────────────────────────────────────────

    #[test]
    fn short_time_returns_na_for_empty_or_zero_time() {
        assert_eq!(short_time(""), "n/a");
        assert_eq!(short_time("0001-01-01T00:00:00Z"), "n/a");
    }

    #[test]
    fn short_time_passes_through_unparseable_input() {
        assert_eq!(short_time("not a timestamp"), "not a timestamp");
    }

    #[test]
    fn short_time_formats_valid_rfc3339_to_local() {
        let s = short_time("2024-06-01T12:00:00Z");
        // Format includes timezone abbreviation; we just check the date part
        // and that it isn't the unparseable passthrough.
        assert!(s.contains("2024-"));
        assert_ne!(s, "2024-06-01T12:00:00Z");
    }

    // ─── saved-name / saved-image round trip ──────────────────────────────

    fn tmp_project_dir(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "casparctl-{}-{}-{}",
            label,
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Drop a Dockerfile so validate_node_project_dir accepts the dir.
        std::fs::write(dir.join("Dockerfile"), "FROM scratch\n").unwrap();
        dir
    }

    #[test]
    fn validate_node_project_dir_requires_dockerfile() {
        let dir = tmp_project_dir("validate-ok");
        validate_node_project_dir(&dir).expect("dockerfile present");
        std::fs::remove_file(dir.join("Dockerfile")).unwrap();
        let err = validate_node_project_dir(&dir).expect_err("missing dockerfile");
        assert!(format!("{}", err).contains("Dockerfile not found"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_then_load_saved_name_round_trip() {
        let dir = tmp_project_dir("name-rt");
        write_saved_name(&dir, "caspar-node").expect("write");
        let name = load_saved_name(&dir).expect("load");
        assert_eq!(name, "caspar-node");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_saved_name_validates_input() {
        let dir = tmp_project_dir("name-validate");
        let err = write_saved_name(&dir, "bad name").expect_err("invalid name");
        assert!(format!("{}", err).contains("allowed characters"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_saved_name_errors_when_missing() {
        let dir = tmp_project_dir("name-missing");
        let err = load_saved_name(&dir).expect_err("file missing");
        assert!(format!("{}", err).contains("could not read"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_then_load_saved_image_round_trip() {
        let dir = tmp_project_dir("image-rt");
        write_saved_image(&dir, "caspar-image-1").expect("write");
        let img = load_saved_image(&dir).expect("load");
        assert_eq!(img, "caspar-image-1");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
