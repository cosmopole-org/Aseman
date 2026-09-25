//! `casparctl run` — bring up a single Caspar node locally, without Docker.
//!
//! The container flow (`casparctl install` + `start`) needs a Docker daemon,
//! gVisor, and the nginx TLS proxy. `run` is the lightweight alternative: it
//! launches the pre-built node binary from `dist/` directly on the host, after
//! generating a fresh single-node config (keys, `.env`, babble genesis) and
//! starting the QuestDB instance the node requires.
//!
//! It is the canonical replacement for the former `run-nodes.sh` local
//! single-node path, minus the privileged gVisor / Firecracker setup, so it
//! works in a plain sandbox.
//!
//! Companion commands:
//!   * `casparctl node-status` — is the node process up, which ports are open
//!   * `casparctl node-stop`   — stop the node (and its QuestDB)
//!
//! The node serves its client transports in **plaintext** (TLS is normally
//! terminated by the nginx proxy). Connect the client CLI with `CASPAR_TLS=0`:
//!   `CASPAR_TLS=0 CASPAR_PROTO=ws CASPAR_PORT=8076 caspar-client login <name>`

use std::fs;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

// ── flag helpers (same convention as the rest of casparctl) ────────────────

fn flag_value(args: &[String], name: &str) -> Option<String> {
    let long = format!("--{}", name);
    let long_eq = format!("--{}=", name);
    let mut i = 0;
    while i < args.len() {
        if args[i] == long {
            return args.get(i + 1).cloned();
        }
        if let Some(v) = args[i].strip_prefix(&long_eq) {
            return Some(v.to_string());
        }
        i += 1;
    }
    None
}

fn has_flag(args: &[String], name: &str) -> bool {
    let long = format!("--{}", name);
    args.iter().any(|a| a == &long)
}

// ── directory resolution ───────────────────────────────────────────────────

/// The prebuilt tree for THIS machine's architecture. `dist/` holds the amd64
/// build and `dist/arm64/` the arm64 one; running an amd64 binary on an arm64
/// host fails at exec ("failed to run caspar-keygen") before anything useful
/// can be said. The choice follows the architecture casparctl itself was built
/// for, which is the host's — it would not be running otherwise.
fn arch_dist(repo: &Path) -> PathBuf {
    if cfg!(target_arch = "aarch64") {
        let arm = repo.join("dist/arm64");
        if arm.join("bin/caspar-node").exists() {
            return arm;
        }
    }
    repo.join("dist")
}

/// Resolve the repo root: the directory that contains `dist/bin/caspar-node`.
fn resolve_repo_dir(args: &[String]) -> Result<PathBuf> {
    if let Some(d) = flag_value(args, "repo-dir") {
        let p = fs::canonicalize(&d).unwrap_or_else(|_| PathBuf::from(&d));
        if p.join("dist/bin/caspar-node").exists() {
            return Ok(p);
        }
        bail!("--repo-dir {} has no dist/bin/caspar-node", p.display());
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let exe = std::env::current_exe().unwrap_or_default();
    let exe_dir = exe.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let mut candidates: Vec<PathBuf> = Vec::new();
    for base in [cwd, exe_dir] {
        let mut b = Some(base);
        while let Some(dir) = b {
            candidates.push(dir.clone());
            b = dir.parent().map(|p| p.to_path_buf());
        }
    }
    for c in candidates {
        if c.join("dist/bin/caspar-node").exists() {
            return Ok(fs::canonicalize(&c).unwrap_or(c));
        }
    }
    bail!("could not locate the repo (dist/bin/caspar-node); pass --repo-dir")
}

fn data_dir(args: &[String], repo: &Path) -> PathBuf {
    if let Some(d) = flag_value(args, "data-dir") {
        return PathBuf::from(d);
    }
    repo.join("caspar-data/node1")
}

fn port_open(port: u16) -> bool {
    TcpStream::connect_timeout(
        &format!("127.0.0.1:{}", port).parse().unwrap(),
        Duration::from_millis(400),
    )
    .map(|s| {
        let _ = s.shutdown(Shutdown::Both);
    })
    .is_ok()
}

fn wait_for_port(port: u16, label: &str, secs: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if port_open(port) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(500));
    }
    bail!("{} did not open port {} within {}s", label, port, secs)
}

// ── config generation ──────────────────────────────────────────────────────

const TCP_PORT: u16 = 8074;
const WS_PORT: u16 = 8076;
const FED_PORT: u16 = 8077;
const CHAIN_PORT: u16 = 8078;
const ENTITY_PORT: u16 = 8079;
const VM_PORT: u16 = 8080;
const TELEMETRY_PORT: u16 = 9099;
const PPROF_PORT: u16 = 9999;
const STORAGE_HTTP_PORT: u16 = 8091;
const QDB_PG: u16 = 8812;
const QDB_HTTP: u16 = 9000;
const QDB_MIN: u16 = 9003;
const QDB_ILP: u16 = 9009;

fn ensure_dirs(dir: &Path) -> Result<()> {
    for sub in [
        "storage",
        "db",
        "applet",
        "search",
        "store_logs",
        "telemetry",
        "babble",
        "questdb",
    ] {
        fs::create_dir_all(dir.join(sub))?;
    }
    Ok(())
}

/// Generate the babble consensus key with the bundled caspar-keygen.
fn gen_babble_key(repo: &Path, dir: &Path) -> Result<()> {
    if dir.join("babble/priv_key").exists() && dir.join("babble/key.pub").exists() {
        return Ok(());
    }
    let keygen = arch_dist(repo).join("bin/caspar-keygen");
    if !keygen.exists() {
        bail!("caspar-keygen not found at {}", keygen.display());
    }
    let tmp = std::env::temp_dir().join(format!("casparctl-keygen-{}", std::process::id()));
    fs::create_dir_all(&tmp)?;
    let status = Command::new(&keygen)
        .env("HOME", &tmp)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to run caspar-keygen at {}", keygen.display()))?;
    // The exit status alone is not trusted either way: the keys are the result.
    let priv_src = tmp.join(".babble/priv_key");
    let pub_src = tmp.join(".babble/key.pub");
    if !priv_src.exists() || !pub_src.exists() {
        let _ = fs::remove_dir_all(&tmp);
        bail!(
            "caspar-keygen at {} ({}) did not produce priv_key + key.pub",
            keygen.display(),
            status
        );
    }
    fs::copy(&priv_src, dir.join("babble/priv_key"))?;
    fs::copy(&pub_src, dir.join("babble/key.pub"))?;
    let _ = fs::remove_dir_all(&tmp);
    Ok(())
}

/// Generate a PKCS#8 RSA owner key via openssl.
fn gen_owner_key() -> Result<String> {
    let genrsa = Command::new("openssl")
        .args(["genrsa", "2048"])
        .output()
        .context("openssl genrsa failed (is openssl installed?)")?;
    if !genrsa.status.success() {
        bail!("openssl genrsa failed");
    }
    let mut pkcs8 = Command::new("openssl")
        .args(["pkcs8", "-topk8", "-nocrypt"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("openssl pkcs8 spawn failed")?;
    pkcs8
        .stdin
        .take()
        .unwrap()
        .write_all(&genrsa.stdout)
        .context("write to openssl pkcs8")?;
    let out = pkcs8.wait_with_output()?;
    if !out.status.success() {
        bail!("openssl pkcs8 conversion failed");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn write_env(dir: &Path, owner_key: &str) -> Result<()> {
    let d = dir.to_string_lossy();
    let env = format!(
        "OWNER_ID=owner-node1\n\
         OWNER_PRIVATE_KEY=\"{owner_key}\"\n\
         STORAGE_ROOT_PATH={d}/storage\n\
         BASE_DB_PATH={d}/db\n\
         APPLET_DB_PATH={d}/applet\n\
         SEARCH_INDEX_PATH={d}/search\n\
         STORE_LOGS_DB={d}/store_logs\n\
         CLIENT_WS_API_PORT={WS_PORT}\n\
         CLIENT_TCP_API_PORT={TCP_PORT}\n\
         FEDERATION_API_PORT={FED_PORT}\n\
         BLOCKCHAIN_API_PORT={CHAIN_PORT}\n\
         ENTITY_API_PORT={ENTITY_PORT}\n\
         VM_API_PORT={VM_PORT}\n\
         PPROF_PORT={PPROF_PORT}\n\
         CASPAR_STORAGE_PORT={STORAGE_HTTP_PORT}\n\
         ORIGIN=http://localhost:{TCP_PORT}\n\
         IPADDR=127.0.0.1\n\
         ROOT_NODE=localhost:{TCP_PORT}\n\
         IS_HEAD=true\n\
         AdminPassword=admin123\n\
         VM_EXEC_COST_PER_SECOND=0\n\
         VM_RAM_COST_PER_MB_PER_MINUTE=0\n\
         VM_CPU_CORE_COST_PER_MINUTE=0\n\
         VM_DISK_COST_PER_GB_PER_MINUTE=0\n\
         TELEMETRY_API_PORT={TELEMETRY_PORT}\n\
         TELEMETRY_DB_PATH={d}/telemetry\n\
         BABBLE_DIR={d}/babble\n\
         BABBLE_DATA_DIR={d}/babble\n\
         CASPAR_BABBLE_FRAME_CACHE=25\n\
         CASPAR_BABBLE_FRAME_RETENTION=25\n\
         QUESTDB_PORT={QDB_PG}\n\
         QUESTDB_HTTP_PORT={QDB_HTTP}\n\
         QUESTDB_HTTP_MIN_PORT={QDB_MIN}\n\
         QUESTDB_ILP_PORT={QDB_ILP}\n\
         QUESTDB_DATA_DIR={d}/questdb\n"
    );
    fs::write(dir.join(".env"), env)?;
    Ok(())
}

/// Single-node babble genesis from the node's own key.pub.
fn write_peers_genesis(dir: &Path) -> Result<()> {
    let pub_hex = fs::read_to_string(dir.join("babble/key.pub"))
        .context("reading babble key.pub")?
        .split_whitespace()
        .collect::<String>()
        .to_uppercase();
    let json = format!(
        "[{{\"NetAddr\":\"127.0.0.1:{CHAIN_PORT}\",\"PubKeyHex\":\"0X{pub_hex}\",\"Moniker\":\"node1\"}}]"
    );
    fs::write(dir.join("babble/peers.genesis.json"), json)?;
    Ok(())
}

fn env_lines(dir: &Path) -> Result<Vec<(String, String)>> {
    // Handles multi-line double-quoted values (e.g. the PEM OWNER_PRIVATE_KEY):
    // when a value opens with `"` and does not close on the same line, keep
    // consuming lines (preserving newlines) until the closing quote.
    let raw = fs::read_to_string(dir.join(".env")).context("reading .env")?;
    let mut out = Vec::new();
    let mut lines = raw.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim().to_string();
        let v = v.trim_start();
        if let Some(rest) = v.strip_prefix('"') {
            // Quoted value — may span multiple lines.
            if let Some(inner) = rest.strip_suffix('"') {
                out.push((key, inner.to_string()));
            } else {
                let mut val = String::from(rest);
                for next in lines.by_ref() {
                    val.push('\n');
                    if let Some(end) = next.strip_suffix('"') {
                        val.push_str(end);
                        break;
                    }
                    val.push_str(next);
                }
                out.push((key, val));
            }
        } else {
            out.push((key, v.trim().to_string()));
        }
    }
    Ok(out)
}

// ── QuestDB ─────────────────────────────────────────────────────────────────

fn resolve_questdb_jar(repo: &Path) -> Option<PathBuf> {
    [
        PathBuf::from("/opt/questdb/questdb.jar"),
        repo.join("dist/questdb/questdb.jar"),
    ]
    .into_iter()
    .find(|path| path.exists())
}

fn start_questdb(repo: &Path, dir: &Path) -> Result<()> {
    if port_open(QDB_PG) {
        println!("• QuestDB already up on {}", QDB_PG);
        return Ok(());
    }
    let jar = resolve_questdb_jar(repo)
        .ok_or_else(|| anyhow!("QuestDB jar not found (dist/questdb/questdb.jar)"))?;
    if Command::new("java").arg("-version").output().is_err() {
        bail!("java not found — QuestDB (required by the node) needs Java 11+");
    }
    let log = fs::File::create(dir.join("questdb.log"))?;
    println!("→ Starting QuestDB (PG={})…", QDB_PG);
    let child = Command::new("java")
        .env("QDB_PG_NET_BIND_TO", format!("0.0.0.0:{}", QDB_PG))
        .env("QDB_HTTP_NET_BIND_TO", format!("0.0.0.0:{}", QDB_HTTP))
        .env("QDB_HTTP_MIN_NET_BIND_TO", format!("0.0.0.0:{}", QDB_MIN))
        .env("QDB_LINE_TCP_NET_BIND_TO", format!("0.0.0.0:{}", QDB_ILP))
        .env("QDB_LINE_UDP_ENABLED", "false")
        .args([
            "-jar",
            &jar.to_string_lossy(),
            "-m",
            "io.questdb/io.questdb.ServerMain",
            "-d",
            &dir.join("questdb").to_string_lossy(),
        ])
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .spawn()
        .context("failed to spawn QuestDB")?;
    fs::write(dir.join("questdb.pid"), child.id().to_string())?;
    wait_for_port(QDB_PG, "QuestDB", 300)?;
    println!("✓ QuestDB ready on {}", QDB_PG);
    Ok(())
}

// ── node launch ─────────────────────────────────────────────────────────────

fn launch_node(repo: &Path, dir: &Path, detach: bool) -> Result<u32> {
    let binary = {
        let dist = arch_dist(repo).join("bin/caspar-node");
        let built = repo.join("target/release/caspar-node");
        if dist.exists() {
            dist
        } else if built.exists() {
            built
        } else {
            bail!("caspar-node binary not found in dist/ or target/release")
        }
    };
    let wasmedge = arch_dist(repo).join("lib/wasmedge");
    let log_path = dir.join("node.log");
    let log = fs::File::create(&log_path)?;

    let mut cmd = Command::new(&binary);
    for (k, v) in env_lines(dir)? {
        cmd.env(k, v);
    }
    let ld = match aseman_config::cli_config().and_then(|config| config.library_path.as_ref()) {
        Some(existing) => format!("{}:{}", wasmedge.display(), existing),
        _ => wasmedge.display().to_string(),
    };
    cmd.env("LD_LIBRARY_PATH", ld)
        // No gVisor here: run docker creatures under stock runc and skip the
        // overlay2+pquota disk quota so container VMs still start.
        .env("CASPAR_DOCKER_RUNTIME", "runc")
        .env("CASPAR_DOCKER_DISK_QUOTA", "0")
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));

    let child = cmd.spawn().context("failed to spawn caspar-node")?;
    let pid = child.id();
    fs::write(dir.join("caspar.pid"), pid.to_string())?;

    wait_for_port(TCP_PORT, "caspar-node", 120)?;
    println!("✓ caspar-node listening (pid {}, TCP {})", pid, TCP_PORT);
    println!("  log: {}", log_path.display());
    if detach {
        // The child keeps running after we return.
        std::mem::forget(child);
    }
    Ok(pid)
}

// ── subcommands ─────────────────────────────────────────────────────────────

/// Verify every host prerequisite the local node needs to run.
fn check_requirements(repo: &Path) -> Result<()> {
    let dist = arch_dist(repo);
    let node_bin = dist.join("bin/caspar-node");
    let built_bin = repo.join("target/release/caspar-node");
    if !node_bin.exists() && !built_bin.exists() {
        bail!("caspar-node binary not found (dist/bin/caspar-node or target/release/caspar-node)");
    }
    {
        let wasmedge_dir = dist.join("lib/wasmedge");
        let link = wasmedge_dir.join("libwasmedge.so.0");
        let lib = if link.exists() {
            link
        } else if wasmedge_dir.join("libwasmedge.so").exists() {
            wasmedge_dir.join("libwasmedge.so")
        } else {
            bail!(
                "bundled WasmEdge library not found under {}",
                wasmedge_dir.display()
            );
        };
        // The real .so is stored in Git LFS. Follow the symlink to the object
        // and confirm it is a genuine ELF library, not an unresolved LFS
        // pointer (the repo was cloned/pulled without Git LFS). Otherwise the
        // node fails at dlopen with a cryptic error.
        let real = fs::canonicalize(&lib).unwrap_or(lib);
        let mut head = [0u8; 4];
        if let Ok(mut f) = fs::File::open(&real) {
            use std::io::Read;
            let _ = f.read(&mut head);
        }
        if head != *b"\x7fELF" {
            bail!(
                "WasmEdge library at {} is not a valid ELF object — it looks like an \
unresolved Git LFS pointer. Install Git LFS and fetch it:\n  git lfs install && git lfs pull",
                real.display()
            );
        }
    }
    if !dist.join("bin/caspar-keygen").exists() {
        bail!(
            "caspar-keygen not found at {}",
            dist.join("bin/caspar-keygen").display()
        );
    }
    if resolve_questdb_jar(repo).is_none() {
        bail!("QuestDB jar not found (dist/questdb/questdb.jar or /opt/questdb/questdb.jar)");
    }
    if Command::new("java").arg("-version").output().is_err() {
        bail!("java not found — QuestDB (required by the node) needs Java 11+");
    }
    if Command::new("openssl").arg("version").output().is_err() {
        bail!("openssl not found — needed to generate the node owner key");
    }
    println!("✓ requirements OK (node binary, WasmEdge lib, keygen, QuestDB jar, java, openssl)");
    Ok(())
}

/// `casparctl install --local` — the one-time local install phase: verify
/// requirements and generate the node's config (keys, `.env`, babble genesis).
/// Does not start anything; run `casparctl run` afterwards.
pub fn install_local(args: &[String]) -> Result<()> {
    if has_flag(args, "help") {
        print_install_local_usage();
        return Ok(());
    }
    let repo = resolve_repo_dir(args)?;
    let dir = data_dir(args, &repo);
    let force = has_flag(args, "force");

    println!("→ Local install for {}", dir.display());
    check_requirements(&repo)?;
    ensure_dirs(&dir)?;

    if dir.join(".env").exists() && !force {
        println!(
            "• Config already present at {} (use --force to regenerate)",
            dir.join(".env").display()
        );
    } else {
        println!("→ Generating fresh single-node config (keys, .env, babble genesis)…");
        gen_babble_key(&repo, &dir)?;
        let owner = gen_owner_key()?;
        write_env(&dir, &owner)?;
        write_peers_genesis(&dir)?;
        println!("✓ config written to {}", dir.join(".env").display());
    }

    println!();
    println!("✓ Local install complete. Start the node with:");
    println!("    casparctl run");
    Ok(())
}

/// Turn the placeholder node owner into a real Caspar creature, then restart the
/// node so it boots as that account.
///
/// `install --local` writes `OWNER_ID=owner-node1` plus a standalone RSA key —
/// an identity the node signs with, but not an account anyone can use (e.g. to
/// mint tokens). Here we register the FIRST creature on the freshly-started node
/// (so it takes creature id `1@<node>`), persist it, and relaunch. Every later
/// start just inherits `OWNER_ID`/`OWNER_PRIVATE_KEY` from `.env`.
///
/// Non-fatal: if registration fails the node keeps running on its placeholder
/// owner and the next `casparctl run` retries.
fn bootstrap_node_owner(repo: &Path, dir: &Path, detach: bool) -> Result<()> {
    let username = aseman_config::cli_config()
        .and_then(|config| config.owner_username.clone())
        .unwrap_or_else(|| super::owner::DEFAULT_OWNER_USERNAME.to_string());
    let email = aseman_config::cli_config()
        .and_then(|config| config.owner_email.clone())
        .filter(|s| s.contains('@'))
        .unwrap_or_else(|| format!("{username}@dev.local"));

    println!("→ Bootstrapping the node owner as a real creature (\"{username}\")…");
    let (owner_id, owner_key) =
        match super::owner::login_creature("127.0.0.1", TCP_PORT, &username, &email) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("! could not create the node-owner creature: {e:#}");
                eprintln!(
                    "  the node keeps its placeholder owner; re-run `casparctl run` to retry"
                );
                return Ok(());
            }
        };
    println!("✓ node owner creature: {owner_id}");

    super::owner::save_owner(dir, &username, &owner_id, &owner_key)
        .context("persisting the node owner (node-owner.json + .env)")?;
    println!(
        "✓ owner id + private key saved to {} and {}",
        dir.join("node-owner.json").display(),
        dir.join(".env").display()
    );

    // The running node still has the placeholder owner in its environment, so
    // restart it to pick up the real one. QuestDB stays up.
    println!("→ Restarting the node so it runs as {owner_id}…");
    if let Some(pid) = read_pid(dir, "caspar.pid") {
        let _ = Command::new("kill").arg(pid.to_string()).status();
        for _ in 0..60 {
            if !pid_alive(pid) {
                break;
            }
            thread::sleep(Duration::from_millis(500));
        }
        if pid_alive(pid) {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
            thread::sleep(Duration::from_secs(2));
        }
    }
    launch_node(repo, dir, detach)?;
    println!("✓ node restarted as owner {owner_id}");
    Ok(())
}

/// `casparctl run` — the run phase: start QuestDB and the node from an
/// already-installed config. Does no installation; run `casparctl install
/// --local` first if the node has not been configured yet.
pub fn run_run(args: &[String]) -> Result<()> {
    if has_flag(args, "help") {
        print_run_usage();
        return Ok(());
    }
    let repo = resolve_repo_dir(args)?;
    let dir = data_dir(args, &repo);
    let detach = has_flag(args, "detach");
    let skip_qdb = has_flag(args, "no-questdb");

    if !dir.join(".env").exists() {
        bail!(
            "no node config at {} — run `casparctl install --local` first",
            dir.display()
        );
    }
    // Keep the babble genesis in sync with the installed key (cheap, idempotent).
    if dir.join("babble/key.pub").exists() {
        let _ = write_peers_genesis(&dir);
    }

    println!("→ Starting Caspar node from {}", dir.display());
    if !skip_qdb {
        start_questdb(&repo, &dir)?;
    }
    launch_node(&repo, &dir, detach)?;

    // First run after an install: turn the placeholder node owner into a REAL
    // Caspar creature (see src/owner.rs). Needs the node serving, hence here.
    if super::owner::needs_bootstrap(&dir) {
        bootstrap_node_owner(&repo, &dir, detach)?;
    } else if let Ok(env) = fs::read_to_string(dir.join(".env"))
        && let Some(id) = env.lines().find_map(|l| {
            l.trim()
                .strip_prefix("OWNER_ID=")
                .map(|v| v.trim().to_string())
        })
    {
        println!("  node owner: {id}");
    }

    println!();
    println!("Caspar node is up. Connect the client CLI (plaintext transport):");
    println!(
        "  CASPAR_TLS=0 CASPAR_PROTO=ws CASPAR_PORT={} caspar-client login <username>",
        WS_PORT
    );
    println!("Check status:  casparctl status");
    println!("Stop it:       casparctl stop");
    Ok(())
}

fn read_pid(dir: &Path, name: &str) -> Option<u32> {
    fs::read_to_string(dir.join(name))
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
}

fn pid_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{}", pid)).exists()
}

pub fn run_status(args: &[String]) -> Result<()> {
    let repo = resolve_repo_dir(args)?;
    let dir = data_dir(args, &repo);
    let node = read_pid(&dir, "caspar.pid");
    match node {
        Some(pid) if pid_alive(pid) => println!("caspar-node: RUNNING (pid {})", pid),
        Some(pid) => println!("caspar-node: not running (stale pid {})", pid),
        None => println!("caspar-node: not started (no {}/caspar.pid)", dir.display()),
    }
    let qdb = read_pid(&dir, "questdb.pid");
    match qdb {
        Some(pid) if pid_alive(pid) => println!("QuestDB:     RUNNING (pid {})", pid),
        _ => println!(
            "QuestDB:     {}",
            if port_open(QDB_PG) {
                "port open"
            } else {
                "not running"
            }
        ),
    }
    println!("Ports:");
    for (label, port) in [
        ("client TCP (TLS)", TCP_PORT),
        ("client WS", WS_PORT),
        ("chain", CHAIN_PORT),
        ("telemetry", TELEMETRY_PORT),
        ("pprof", PPROF_PORT),
    ] {
        println!(
            "  {:<18} {:<6} {}",
            label,
            port,
            if port_open(port) { "OPEN" } else { "closed" }
        );
    }
    // A quick liveness probe against the telemetry snapshot.
    if port_open(TELEMETRY_PORT)
        && let Ok(body) = http_get(TELEMETRY_PORT, "/telemetry/snapshot")
    {
        let n = body.len();
        println!("telemetry snapshot: {} bytes", n);
    }
    Ok(())
}

/// Stop a locally-run node (and its QuestDB) if one is present. Returns whether
/// anything was stopped, so the top-level `stop` command can fall back to the
/// Docker-container flow when there is no local node.
pub fn stop_local(args: &[String]) -> Result<bool> {
    // A local node only exists next to a dist/ tree; if we can't resolve one,
    // there is nothing native to stop — let the caller try the Docker flow.
    let repo = match resolve_repo_dir(args) {
        Ok(r) => r,
        Err(_) => return Ok(false),
    };
    let dir = data_dir(args, &repo);
    let mut stopped = false;
    for (name, label) in [("caspar.pid", "caspar-node"), ("questdb.pid", "QuestDB")] {
        if let Some(pid) = read_pid(&dir, name) {
            if pid_alive(pid) {
                let _ = Command::new("kill").arg(pid.to_string()).status();
                println!("✓ stopped {} (pid {})", label, pid);
                stopped = true;
            }
            let _ = fs::remove_file(dir.join(name));
        }
    }
    Ok(stopped)
}

/// Minimal HTTP GET for the telemetry liveness probe (no external deps).
fn http_get(port: u16, path: &str) -> Result<String> {
    let mut stream = TcpStream::connect_timeout(
        &format!("127.0.0.1:{}", port).parse().unwrap(),
        Duration::from_millis(800),
    )?;
    stream.set_read_timeout(Some(Duration::from_millis(1500)))?;
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        path
    );
    stream.write_all(req.as_bytes())?;
    let mut buf = String::new();
    let _ = stream.read_to_string(&mut buf);
    let _ = stream.shutdown(Shutdown::Both);
    if let Some(idx) = buf.find("\r\n\r\n") {
        Ok(buf[idx + 4..].to_string())
    } else {
        Ok(buf)
    }
}

fn print_install_local_usage() {
    println!(
        "casparctl install --local - one-time local install (no Docker)\n\n\
         Usage:\n  casparctl install --local [flags]\n\n\
         Flags:\n  \
         --repo-dir PATH   repo root containing dist/ (auto-detected)\n  \
         --data-dir PATH   node data directory (default <repo>/caspar-data/node1)\n  \
         --force           regenerate the config even if it already exists\n\n\
         Verifies the host requirements (node binary, bundled WasmEdge library,\n\
         caspar-keygen, QuestDB jar, Java, openssl) and generates the node's\n\
         config once: babble consensus key, PKCS#8 owner key, .env, and the\n\
         babble peers.genesis.json. Starts nothing — run `casparctl run` next."
    );
}

fn print_run_usage() {
    println!(
        "casparctl run - start the installed local Caspar node (no Docker)\n\n\
         Usage:\n  casparctl run [flags]\n\n\
         Flags:\n  \
         --repo-dir PATH   repo root containing dist/ (auto-detected)\n  \
         --data-dir PATH   node data directory (default <repo>/caspar-data/node1)\n  \
         --detach          keep the node running after this command returns\n  \
         --no-questdb      do not start QuestDB (assume it is already running)\n\n\
         Starts QuestDB and the pre-built dist/ node (with the bundled WasmEdge\n\
         library on the loader path) from a config produced by\n\
         `casparctl install --local`. Does no installation itself.\n\n\
         The node serves plaintext client transports (TLS is normally handled\n\
         by an nginx proxy). Connect the client CLI with CASPAR_TLS=0."
    );
}
