// Caspar node — Rust translation of the Caspar (kasper) Go node.

#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(clippy::module_inception)]
#![allow(clippy::type_complexity)]

#[macro_use]
mod compat;

mod bots;
mod core;
mod drivers;
mod models;
mod shell;
mod telemetry;
mod util;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::models::action::ExtendedField;
use crate::models::core::ICore;
use crate::models::transaction::ITrx;
use crate::shell::api::main_api::plug_all;
use crate::shell::kasper::new_configured_app;
use aseman_config::{AllocatorConfig, AsemanConfig};

/// Run the legacy node composition while use cases move behind Aseman ports.
///
/// New binaries call this compatibility entry point; it expires after the node
/// composition has moved to `apps/aseman-node` and its removal gate passes.
pub fn run() {
    // Compatibility only: legacy adapters still consume process variables. The parser
    // is owned by aseman-config; this process mutation expires as those adapters accept
    // typed configuration directly.
    let _ = install_dotenv_compat(".env");
    let config = match AsemanConfig::from_process_with_dotenv(".env") {
        Ok(config) => Arc::new(config),
        Err(error) => {
            eprintln!("invalid Aseman configuration: {error}");
            return;
        }
    };
    if let Err(error) = aseman_config::install_legacy_adapter_snapshot(&config) {
        eprintln!("could not install Aseman configuration snapshot: {error}");
        return;
    }

    // Cap glibc's per-thread arena pool and keep freed pages returning to the
    // OS. Must run before any worker thread is spawned (pprof/telemetry below
    // both spawn), so glibc never grows past the cap. See
    // `configure_allocator` for the leak this addresses.
    configure_allocator(&config.allocator);

    // Runtime profiling HTTP server (was Go `net/http/pprof` on :9999;
    // now Rust-native via the `pprof` crate). Queried by `casparctl pprof`.
    telemetry::pprof::start(config.telemetry.pprof_port);

    if let Err(e) = telemetry::start(&config) {
        eprintln!("telemetry server start failed: {}", e);
    }

    let owner_priv = match parse_owner_key(&config.node.private_key_secret) {
        Some(k) => k,
        None => {
            eprintln!("ASEMAN_NODE_PRIVATE_KEY_SECRET missing or unparseable");
            return;
        }
    };
    let app = new_configured_app(
        &config.node.origin,
        &config.node.id,
        owner_priv,
        config.clone(),
    );

    if let Err(e) = app.load_inner(
        vec!["keyhan".to_string()],
        &config.storage.root_path,
        &config.storage.base_db_path,
        &config.storage.applet_db_path,
        &config.storage.store_logs_db,
        &config.storage.search_index_path,
    ) {
        eprintln!("app.load failed: {}", e);
        return;
    }

    // Install SIGINT / SIGTERM handler: when received, close the app and
    // exit. We use a small helper instead of pulling in signal-hook.
    install_signal_handler({
        let app = app.clone();
        move || {
            app.close();
            std::process::exit(0);
        }
    });

    let mut user_extender: HashMap<String, ExtendedField> = HashMap::new();
    let make_field =
        |name: &str, default: serde_json::Value, searchable: bool, primary: bool| ExtendedField {
            name: name.to_string(),
            path: "metadata.public.profile".to_string(),
            typ: "string".to_string(),
            default,
            required: true,
            searchable,
            primary_prop: primary,
            get_value: None,
        };
    user_extender.insert(
        "name".to_string(),
        make_field("name", serde_json::json!("Anonymous User"), true, true),
    );
    user_extender.insert(
        "avatar".to_string(),
        make_field("avatar", serde_json::json!("avatar"), false, true),
    );
    user_extender.insert(
        "bio".to_string(),
        make_field(
            "bio",
            serde_json::json!("I'm a DecillionAI User"),
            false,
            false,
        ),
    );
    user_extender.insert(
        "location".to_string(),
        make_field(
            "location",
            serde_json::json!("DecillionAI Land"),
            false,
            false,
        ),
    );
    let mut store_extender: HashMap<String, ExtendedField> = HashMap::new();
    store_extender.insert(
        "title".to_string(),
        make_field("title", serde_json::json!("Untitled Store"), true, true),
    );
    store_extender.insert(
        "avatar".to_string(),
        make_field("avatar", serde_json::json!("avatar"), false, true),
    );
    let mut model_extender: HashMap<String, HashMap<String, ExtendedField>> = HashMap::new();
    model_extender.insert("user".to_string(), user_extender);
    model_extender.insert("store".to_string(), store_extender);

    let app_for_plug: Arc<dyn crate::models::core::ICore> = app.clone();
    plug_all(app_for_plug, &model_extender);

    // ── Startup VMM listener restore ──────────────────────────────────────────
    // The signaler listeners that vmm.assign() registers are in-memory only.
    // After a node restart they are gone, so creature signals to deployed
    // machines would silently drop.  Scan the DB for all previously deployed
    // entity type links and re-register one listener per unique machine_id.
    {
        use std::collections::HashSet;
        let keys_slot = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
        let keys_clone = keys_slot.clone();
        app.modify_state(
            true,
            Box::new(move |trx: &dyn crate::models::transaction::ITrx| {
                *keys_clone.lock().unwrap() = trx.get_by_prefix("link::vmEntityType::");
                Ok(())
            }),
        );
        let mut seen: HashSet<String> = HashSet::new();
        for key in keys_slot.lock().unwrap().iter() {
            // key = "link::vmEntityType::MACHINE_ID::ENTITY_ID"
            let rest = key.strip_prefix("link::vmEntityType::").unwrap_or("");
            if let Some(machine_id) = rest.split("::").next() {
                if !machine_id.is_empty() && seen.insert(machine_id.to_string()) {
                    app.tools().vmm().assign(machine_id);
                }
            }
        }
        if !seen.is_empty() {
            eprintln!(
                "[startup] Restored VMM listeners for {} machine(s): {:?}",
                seen.len(),
                seen
            );
        }
    }

    app.run();

    // ── Geo-distributed cluster mesh ──────────────────────────────────────────
    // When cluster mode is enabled (cluster.json / CLUSTER_* env), this node
    // joins the OpenRaft mesh of same-origin instances: shell API state and
    // distributed-mode creature deployments replicate to every instance, and
    // the cluster HTTP listener serves the raft RPC + `casparctl cluster`
    // orchestration API. Standalone nodes skip this entirely.
    {
        let app_for_cluster: Arc<dyn crate::models::core::ICore> = app.clone();
        drivers::cluster::init(app_for_cluster, &config.cluster);
    }

    // ── Docker-host bridge gateway ────────────────────────────────────────────
    // Long-lived TCP server that docker-based creature containers connect to.
    // It is their only channel to the outside world: every host interaction
    // (DB/storage ops, outbound HTTP, signalling) and every inbound signal flows
    // over it. Disabled when the port is unset/zero.
    app.tools()
        .vmm()
        .start_docker_gateway(i64::from(config.network.docker_gateway_port));

    // ── VMM HTTP ingress ──────────────────────────────────────────────────────
    // Inbound HTTP server that accepts requests shaped as
    // `/{creatureId}/{programId}/{entityId}/{vmId}/{path…}` and forwards them to
    // the HTTP server of the named VM instance: the docker runtime proxies to
    // the container's HTTP server, every other runtime falls back to signalling
    // the VM. Disabled when the port is unset/zero.
    app.tools()
        .vmm()
        .start_http_ingress(i64::from(config.network.vm_http_ingress_port));

    // ── Public file storage HTTP server ───────────────────────────────────────
    // Serves public binary blobs (avatars/images) over plain HTTP so the Nest
    // backend can proxy authenticated uploads and re-serve downloads to clients
    // without pushing binaries through the signed action/consensus path.
    // Internal port (like the docker gateway); disabled when unset/zero.
    {
        let app_for_storage: Arc<dyn crate::models::core::ICore> = app.clone();
        crate::shell::storage_http::start(
            app_for_storage,
            i64::from(config.network.public_storage_port),
            config.legacy_adapters.public_storage_max_bytes,
        );
    }

    let mut ports: HashMap<String, i64> = HashMap::new();
    ports.insert("tcp".to_string(), i64::from(config.network.legacy_tcp_port));
    ports.insert("ws".to_string(), i64::from(config.network.legacy_ws_port));
    ports.insert(
        "fed".to_string(),
        i64::from(config.network.legacy_federation_port),
    );
    ports.insert(
        "chain".to_string(),
        i64::from(config.network.legacy_consensus_port),
    );
    app.tools().network().run(ports);

    // Periodically hand freed heap pages back to the OS (see
    // `configure_allocator`). Cheap once arenas are capped.
    spawn_malloc_trimmer(&config.allocator);

    // Block forever — background threads run the gossip / chain dispatch.
    loop {
        thread::sleep(Duration::from_secs(60 * 60));
    }
}

/// glibc allocator tuning to stop unbounded RSS growth under sustained load.
///
/// Every inbound wasm signal is executed on a freshly `thread::spawn`ed worker
/// (plus a watchdog thread) — see `vms/wasm/src/controller.rs`. glibc's malloc
/// gives each thread that contends for the main arena its own 64 MiB secondary
/// arena, up to `8 * ncpu` of them by default (32 on a 4-core box ≈ 2 GiB).
/// Those arenas are pooled and reused when the thread exits, but their resident
/// pages are never returned once grown, and their trim threshold drifts upward
/// after frees. The result is RSS that climbs monotonically over a day of
/// signalling — the memory the process reported growing ~1.2 GiB over 12 h,
/// even though the malloc heap itself (per massif) stays flat: the bytes are
/// freed but retained by the allocator.
///
/// Two knobs fix it without touching the per-signal threading model:
///   * `M_ARENA_MAX` caps the number of arenas so the pool can't balloon.
///   * `M_TRIM_THRESHOLD` fixed low keeps top-of-heap trimming responsive
///     instead of letting glibc raise the threshold after large frees.
/// Combined with the periodic `malloc_trim` below, resident memory now tracks
/// live allocations instead of the high-water mark.
///
/// Both are tunable via env (`CASPAR_MALLOC_ARENA_MAX`, default 2) so an
/// operator can widen or disable the cap without a rebuild. glibc-only; a no-op
/// on other libcs.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn configure_allocator(config: &AllocatorConfig) {
    // glibc mallopt parameter numbers (not exported by the libc crate on all
    // versions, so spell them out — they are stable ABI).
    const M_TRIM_THRESHOLD: libc::c_int = -1;
    const M_ARENA_TEST: libc::c_int = -7;
    const M_ARENA_MAX: libc::c_int = -8;

    let arena_max: libc::c_int = config.arena_max;

    unsafe {
        if arena_max > 0 {
            // Hard cap on the arena pool, plus the "start testing" threshold so
            // glibc never provisions beyond the cap in the first place.
            libc::mallopt(M_ARENA_MAX, arena_max);
            libc::mallopt(M_ARENA_TEST, arena_max);
        }
        // Keep the main-arena trim threshold pinned low (128 KiB) so sbrk space
        // is released promptly rather than after the threshold auto-grows.
        libc::mallopt(M_TRIM_THRESHOLD, 128 * 1024);
    }

    eprintln!(
        "[startup] allocator: M_ARENA_MAX={} (0 = unchanged), trim threshold pinned",
        arena_max
    );
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn configure_allocator(_config: &AllocatorConfig) {}

/// Background thread that calls `malloc_trim(0)` on an interval, returning the
/// freed tops of every (now capped) arena to the OS via `madvise`. Interval is
/// `CASPAR_MALLOC_TRIM_SECS` (default 30); 0 disables it. glibc-only.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn spawn_malloc_trimmer(config: &AllocatorConfig) {
    let secs = config.trim_interval_seconds;
    if secs == 0 {
        return;
    }
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(secs));
        unsafe {
            libc::malloc_trim(0);
        }
    });
    eprintln!("[startup] allocator: malloc_trim every {}s", secs);
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn spawn_malloc_trimmer(_config: &AllocatorConfig) {}

fn parse_owner_key(pem: &str) -> Option<rsa::RsaPrivateKey> {
    use rsa::pkcs8::DecodePrivateKey;
    rsa::RsaPrivateKey::from_pkcs8_pem(pem).ok()
}

fn install_dotenv_compat(path: &str) -> Result<(), aseman_config::ConfigError> {
    let content = std::fs::read_to_string(path)
        .map_err(|error| aseman_config::ConfigError::DotenvIo(error.to_string()))?;
    for (key, value) in aseman_config::parse_dotenv(&content)? {
        // SAFETY: this runs at the composition root before any thread is spawned.
        unsafe {
            std::env::set_var(key, value);
        }
    }
    Ok(())
}

fn install_signal_handler<F: FnOnce() + Send + 'static>(callback: F) {
    // Minimal SIGINT / SIGTERM handling without signal-hook: spawn a thread
    // that masks the signals and waits via `libc::sigwait`. Linux only —
    // matches the Caspar deployment target.
    thread::spawn(move || {
        #[cfg(target_os = "linux")]
        unsafe {
            let mut mask: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            libc::sigaddset(&mut mask, libc::SIGINT);
            libc::sigaddset(&mut mask, libc::SIGTERM);
            libc::pthread_sigmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut());
            let mut sig: libc::c_int = 0;
            libc::sigwait(&mask, &mut sig);
        }
        callback();
    });
}
