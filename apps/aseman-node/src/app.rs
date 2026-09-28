//! Node composition root (RL-001).
//!
//! [`NodeApp`] is where configuration is parsed and the adapters are wired together.
//! It is the single place a new node process is brought up; the binaries in `main.rs`
//! and the `run` entry point are thin callers. Business rules and environment reads
//! stay out of this module — typed configuration comes from `aseman-config`, and the
//! adapters it wires sit behind the ports in `aseman-ports`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use aseman_config::{AllocatorConfig, AsemanConfig};

use crate::api::kasper::new_configured_app;
use crate::api::main_api::plug_all;
use crate::models::action::ExtendedField;
use crate::models::core::ICore;
use crate::observability;

/// Connections one node keeps for PostgreSQL units of work (one per concurrent
/// action, plus nesting).
const CORE_STORAGE_CONNECTIONS: u32 = 16;

/// The composed node: a typed configuration plus everything wired from it.
pub struct NodeApp {
    config: Arc<AsemanConfig>,
}

impl NodeApp {
    /// Parse the process configuration (with the legacy `.env` precedence) and install
    /// the snapshot legacy leaf adapters still read.
    pub fn from_process() -> Result<Self> {
        // Compatibility only: legacy adapters still consume process variables. The parser
        // is owned by aseman-config; this process mutation expires as those adapters accept
        // typed configuration directly.
        let _ = install_dotenv_compat(".env");
        let config = Arc::new(AsemanConfig::from_process_with_dotenv(".env")?);
        aseman_config::install_legacy_adapter_snapshot(&config)?;
        Ok(Self { config })
    }

    /// Bring the node up: profiler, telemetry, core storage, the legacy app, the VMM
    /// listener restore, and the network/cluster ingress. Blocks forever once running.
    pub fn start(self) -> Result<()> {
        let config: &AsemanConfig = &self.config;

        // Cap glibc's per-thread arena pool and keep freed pages returning to the
        // OS. Must run before any worker thread is spawned (pprof/telemetry below
        // both spawn), so glibc never grows past the cap. See
        // `configure_allocator` for the leak this addresses.
        configure_allocator(&config.allocator);

        // Runtime profiling HTTP server (was Go `net/http/pprof` on :9999;
        // now Rust-native via the `pprof` crate). Queried by `casparctl pprof`.
        observability::pprof::start(config.telemetry.pprof_port);

        if let Err(e) = observability::start(config) {
            eprintln!("telemetry server start failed: {}", e);
        }

        if let Err(error) = install_core_storage(config) {
            eprintln!("core storage could not start: {error}");
            return Err(anyhow::anyhow!("core storage could not start: {error}"));
        }

        let owner_priv = match parse_owner_key(&config.node.private_key_secret) {
            Some(key) => key,
            None => {
                return Err(anyhow::anyhow!(
                    "ASEMAN_NODE_PRIVATE_KEY_SECRET missing or unparseable"
                ));
            }
        };
        let app = new_configured_app(
            &config.node.origin,
            &config.node.id,
            owner_priv,
            self.config.clone(),
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
            return Err(anyhow::anyhow!("app.load failed: {e}"));
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
        let make_field = |name: &str,
                          default: serde_json::Value,
                          searchable: bool,
                          primary: bool| ExtendedField {
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
        // machines would silently drop. Re-register one listener per program with a
        // deployed entity.
        {
            let programs_slot = Arc::new(Mutex::new(Vec::<String>::new()));
            let programs_clone = programs_slot.clone();
            let blobs = crate::adapters::blob_store::node_blobs(&*app.tools().storage());
            app.modify_state(
                true,
                Box::new(move |trx: &dyn crate::models::transaction::ITrx| {
                    let entities =
                        crate::api::model::entity_ports::EntityPorts { trx, blobs: &blobs };
                    *programs_clone.lock().unwrap() =
                        aseman_ports::EntityDirectory::deployed_programs(&entities)
                            .map_err(|error| anyhow::anyhow!("{error}"))?;
                    Ok(())
                }),
            );
            let programs = programs_slot.lock().unwrap().clone();
            for program_id in &programs {
                app.tools().workloads().assign(program_id);
            }
            if !programs.is_empty() {
                eprintln!(
                    "[startup] Restored VMM listeners for {} machine(s): {:?}",
                    programs.len(),
                    programs
                );
            }
        }

        app.run();

        // ── VMM HTTP ingress ──────────────────────────────────────────────────────
        // Inbound HTTP server that accepts requests shaped as
        // `/{creatureId}/{programId}/{entityId}/{vmId}/{path…}` and forwards them to
        // the HTTP server of the named VM instance: the docker runtime proxies to
        // the container's HTTP server, every other runtime falls back to signalling
        // the VM. Disabled when the port is unset/zero.
        app.tools()
            .workloads()
            .start_http_ingress(i64::from(config.network.vm_http_ingress_port));

        // ── Public file storage HTTP server ───────────────────────────────────────
        // Serves public binary blobs (avatars/images) over plain HTTP so the Nest
        // backend can proxy authenticated uploads and re-serve downloads to clients
        // without pushing binaries through the signed action/consensus path.
        // Internal port (like the docker gateway); disabled when unset/zero.
        {
            let app_for_storage: Arc<dyn crate::models::core::ICore> = app.clone();
            crate::api::storage_http::start(
                app_for_storage,
                i64::from(config.network.public_storage_port),
                config.legacy_adapters.public_storage_max_bytes,
            );
        }

        // ── Public HTTP gateway (RL-004) ─────────────────────────────────────────
        // Serves the generated A701 public contract over TLS when the
        // ASEMAN_PUBLIC_HTTP_* configuration is present. A no-op otherwise.
        if let Err(error) = crate::api::public_http::start_public_http(config, app.clone()) {
            eprintln!("public HTTP gateway could not start: {error}");
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
}

/// Select the provider of the core port families (ADR 0026). Legacy needs nothing;
/// PostgreSQL is migrated and installed before any state action runs, with every
/// write fenced at the configured binding generation (A309).
fn install_core_storage(config: &AsemanConfig) -> Result<()> {
    use aseman_config::CoreStorageProvider;
    if config.core_storage.provider == CoreStorageProvider::RocksDb {
        return Ok(());
    }
    let secret = config
        .database_url_secret
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("ASEMAN_DATABASE_URL_SECRET is required"))?;
    let url = aseman_config::read_secret_file(secret, 4096)?;
    // The configured capsule layout (ADR 0034) is recorded in the database, so every
    // repository and unit of work opened below writes in it.
    let layout = config.core_storage.layout;
    aseman_storage_postgres::PostgresCapsuleRepository::connect(&url)?.migrate_layout(layout)?;
    // Cluster mode (ADR 0033): capsules shard and replicate across the map's shards;
    // `ASEMAN_DATABASE_URL_SECRET` names the home shard, which also holds coordination
    // and the compatibility state. Otherwise one database serves everything.
    let generation = Some(config.core_storage.binding_generation);
    let factory: std::sync::Arc<dyn aseman_storage_postgres::shard::UnitOfWorkFactory> =
        match &config.core_storage.postgres_shards_secret {
            Some(secret) => {
                let map = aseman_storage_postgres::shard::ShardMap::parse(
                    &aseman_config::read_secret_file(secret, 64 * 1024)?,
                )?;
                eprintln!(
                    "[storage] PostgreSQL cluster: {} shard(s), shard map v{}",
                    map.shards.len(),
                    map.version
                );
                std::sync::Arc::new(std::sync::Arc::new(
                    aseman_storage_postgres::shard::ShardedUnitOfWorkFactory::connect(
                        map,
                        CORE_STORAGE_CONNECTIONS,
                        generation,
                        layout,
                    )?,
                ))
            }
            None => std::sync::Arc::new(
                aseman_storage_postgres::unit_of_work::PostgresUnitOfWorkFactory::connect(
                    &url,
                    CORE_STORAGE_CONNECTIONS,
                    generation,
                )?,
            ),
        };
    crate::api::model::core_storage::install_postgres(factory)?;
    // Guest data moves with the core families: each creature's own database, through
    // the trusted guest proxy (ADR 0021, A405).
    let proxy = config
        .core_storage
        .guest_proxy
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("the guest proxy is required on PostgreSQL"))?;
    let proxy_url = aseman_config::read_secret_file(&proxy.url_secret, 4096)?;
    let mut router = aseman_storage_postgres::guest::GuestPoolRouter::new(
        &proxy_url,
        &proxy.role,
        proxy.max_pools,
        proxy.max_pool_size,
    )
    .map_err(|error| anyhow::anyhow!("{error}"))?;
    // In cluster mode a creature's guest databases live on its shard's server.
    if let Some(secret) = &config.core_storage.postgres_shards_secret {
        let map = aseman_storage_postgres::shard::ShardMap::parse(
            &aseman_config::read_secret_file(secret, 64 * 1024)?,
        )?;
        for shard in &map.shards {
            if let Some(guest_proxy) = &shard.guest_proxy {
                router = router
                    .with_shard_proxy(&shard.name, guest_proxy)
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
            }
        }
    }
    crate::api::audit::install_postgres(
        aseman_storage_postgres::PostgresCapsuleRepository::connect(&url)?,
    )?;
    crate::api::model::guest_data::install_postgres(
        aseman_storage_postgres::guest::PostgresGuestKv::new(router),
        aseman_storage_postgres::PostgresCapsuleRepository::connect(&url)?,
    )?;
    // Program entities run on the configured VMM; their host calls come back through
    // the guest API (P5-03, P5-04).
    if let Some(vmm) = &config.vmm {
        crate::api::workloads::install(vmm, &config.node.id, &url, &config.storage.root_path)?;
    }
    Ok(())
}

/// glibc allocator tuning to stop unbounded RSS growth under sustained load.
///
/// Every inbound wasm signal is executed on a freshly `thread::spawn`ed worker
/// (plus a watchdog thread) — see `modules/runtime/wasm/src/controller.rs`. glibc's malloc
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
///     Combined with the periodic `malloc_trim` below, resident memory now tracks
///     live allocations instead of the high-water mark.
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
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_secs(secs));
            unsafe {
                libc::malloc_trim(0);
            }
        }
    });
    eprintln!("[startup] allocator: malloc_trim every {}s", secs);
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn spawn_malloc_trimmer(_config: &AllocatorConfig) {}

/// `ASEMAN_NODE_PRIVATE_KEY_SECRET` is a secret reference: the path of a PKCS#8 PEM
/// file.
fn parse_owner_key(secret: &str) -> Option<rsa::RsaPrivateKey> {
    use rsa::pkcs8::DecodePrivateKey;
    rsa::RsaPrivateKey::from_pkcs8_pem(&std::fs::read_to_string(secret).ok()?).ok()
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

#[cfg(test)]
mod owner_key_tests {
    use super::parse_owner_key;
    use rsa::pkcs8::{EncodePrivateKey, LineEnding};

    #[test]
    fn owner_key_is_read_from_its_secret_file_only() {
        let key = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 1024).unwrap();
        let pem = key.to_pkcs8_pem(LineEnding::LF).unwrap().to_string();
        let path = std::env::temp_dir().join(format!("owner-key-{}.pem", std::process::id()));
        std::fs::write(&path, &pem).unwrap();
        assert_eq!(parse_owner_key(path.to_str().unwrap()).as_ref(), Some(&key));
        assert!(
            parse_owner_key(&pem).is_none(),
            "an inline PEM is not a secret reference"
        );
        assert!(parse_owner_key("/nonexistent/owner-key.pem").is_none());
        std::fs::remove_file(path).unwrap();
    }
}
