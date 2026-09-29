//! The node's composition root.
//!
//! [`NodeApp`] is where configuration is parsed and the adapters are wired together.
//! It is the single place a new node process is brought up; the binaries in `main.rs`
//! and the `run` entry point are thin callers. Business rules and environment reads
//! stay out of this module — typed configuration comes from `aseman-config`, and the
//! adapters it wires sit behind the ports in `aseman-ports`.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use aseman_config::{AllocatorConfig, AsemanConfig};

use crate::actions::Router;
use crate::node::Node;
use crate::observability;

/// The composed node: a typed configuration plus everything wired from it.
pub struct NodeApp {
    config: Arc<AsemanConfig>,
}

impl NodeApp {
    /// Parse the process configuration: the environment, overridden by `.env`.
    pub fn from_process() -> Result<Self> {
        let config = Arc::new(AsemanConfig::from_process_with_dotenv(".env")?);
        Ok(Self { config })
    }

    /// Bring the node up: profiler, telemetry, the node and its components, the
    /// operations, the workload services, and the listeners. Blocks while the node
    /// runs.
    ///
    /// # Errors
    ///
    /// A missing owner key, or a component that cannot start.
    pub fn start(self) -> Result<()> {
        let config = self.config.clone();

        // Cap glibc's arena pool before any worker thread is spawned (the profiler
        // and telemetry below both spawn); see `configure_allocator`.
        configure_allocator(&config.allocator);
        observability::pprof::start(config.telemetry.pprof_port);
        if let Err(error) = observability::start(&config) {
            eprintln!("telemetry server start failed: {error}");
        }

        let owner_key = parse_owner_key(&config.node.private_key_secret).ok_or_else(|| {
            anyhow::anyhow!("ASEMAN_NODE_PRIVATE_KEY_SECRET missing or unparseable")
        })?;
        let node = Node::new(config.clone(), owner_key);
        node.load()
            .map_err(|error| anyhow::anyhow!("the node could not load: {error}"))?;
        install_core_storage(&config, &node)
            .map_err(|error| anyhow::anyhow!("core storage services could not start: {error}"))?;
        let router = Router::new(node.clone())?;
        node.install_router(router.clone());

        install_signal_handler({
            let node = node.clone();
            move || {
                node.close();
                std::process::exit(0);
            }
        });

        crate::actions::install_creature_types(&node)?;
        crate::actions::start_workload_services(&node)?;

        // Inbound HTTP for VM instances: `/{creatureId}/{programId}/{entityId}/{vmId}/…`
        // and custom gateway routes (disabled when the port is zero).
        node.tools()
            .workloads()
            .start_http_ingress(i64::from(config.network.vm_http_ingress_port));
        // Public blobs (avatars, images) over plain HTTP on an internal port, so a
        // backend can proxy authenticated uploads and serve downloads (disabled when
        // the port is zero).
        crate::transports::storage_http::start(
            node.clone(),
            i64::from(config.network.public_storage_port),
            config.services.public_storage_max_bytes,
        );
        // The public contract over TLS (A701), when `ASEMAN_PUBLIC_HTTP_*` is set.
        if let Err(error) = crate::transports::http::start_public_http(&config, router) {
            eprintln!("public HTTP gateway could not start: {error}");
        }

        let ports = HashMap::from([
            ("tcp".to_owned(), i64::from(config.network.tcp_port)),
            ("ws".to_owned(), i64::from(config.network.ws_port)),
            ("fed".to_owned(), i64::from(config.network.federation_port)),
            ("chain".to_owned(), i64::from(config.network.chain_port)),
        ]);
        node.tools().network().run(ports);

        // Hand freed heap pages back to the OS periodically (see `configure_allocator`).
        spawn_malloc_trimmer(&config.allocator);

        // The listeners and the chain run on their own threads.
        loop {
            thread::sleep(Duration::from_secs(60 * 60));
        }
    }
}

/// Compose the services that run on the node's storage once it is open (ADR 0036):
/// decision audit, guest data (from each creature's own database through the
/// PostgreSQL guest data plane, ADR 0021, or else from the node's storage), and the
/// VMM workload catalog.
fn install_core_storage(config: &AsemanConfig, node: &Arc<Node>) -> Result<()> {
    let storage = node.tools().storage().storage();
    node.audit().start(storage.clone())?;
    let guest_plane = match &config.core_storage.guest_proxy {
        Some(proxy) => {
            let url = aseman_config::read_secret_file(&proxy.url_secret, 4096)?;
            let shard_map = match &config.core_storage.postgres_shards_secret {
                Some(secret) => Some(aseman_config::read_secret_file(secret, 64 * 1024)?),
                None => None,
            };
            let kv = aseman_storage_providers::guest_kv(&aseman_storage_providers::GuestProxy {
                url: &url,
                role: &proxy.role,
                max_pools: proxy.max_pools,
                max_pool_size: proxy.max_pool_size,
                shard_map: shard_map.as_deref(),
            })
            .map_err(|error| anyhow::anyhow!("{error}"))?;
            Some(kv)
        }
        None => None,
    };
    let _ = node
        .guest_data
        .set(crate::state::guest_data::GuestData::new(
            guest_plane,
            storage.clone(),
        ));
    // Program entities run on the configured VMM; their host calls come back through
    // the guest API.
    if let Some(vmm) = &config.vmm {
        let remote = crate::workloads::vmm::install(vmm, node, storage, &config.storage.root_path)?;
        let _ = node.vmm.set(remote);
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

fn install_signal_handler<F: FnOnce() + Send + 'static>(callback: F) {
    // Minimal SIGINT / SIGTERM handling without signal-hook: spawn a thread
    // that masks the signals and waits via `libc::sigwait`. Linux only —
    // the deployment target.
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
