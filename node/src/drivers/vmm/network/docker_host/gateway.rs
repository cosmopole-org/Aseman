//! The [`DockerHostGateway`] object — the owning instance for the docker-host
//! bridge.
//!
//! There is **no process-wide gateway static**. A single `DockerHostGateway` is
//! constructed and held by the `Vmm` driver and reached through the canonical
//! `ICore → tools() → vmm()` object graph, in keeping with the rest of the VMM
//! subsystem. It owns:
//!
//! * the live-connection [`GatewayRegistry`], and
//! * an [`ICore`] handle used to resolve session tokens to a VM's authoritative
//!   identity (`tools().vmm().resolve_vm_session`) and to dispatch host calls.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::drivers::vmm::network::docker_host::connection::GatewayRegistry;
use crate::drivers::vmm::network::docker_host::server::run_connection;
use crate::drivers::vmm::prelude::*;
use crate::models::core::ICore;

use std::net::TcpListener;

pub(crate) struct DockerHostGateway {
    /// Core handle for token resolution and host-call dispatch.
    pub(crate) app: Arc<dyn ICore>,
    /// Live container connections.
    pub(crate) registry: GatewayRegistry,
    /// Guards against starting the listener twice.
    listening: AtomicBool,
}

impl DockerHostGateway {
    pub(crate) fn new(app: Arc<dyn ICore>) -> Arc<DockerHostGateway> {
        Arc::new(DockerHostGateway {
            app,
            registry: GatewayRegistry::new(),
            listening: AtomicBool::new(false),
        })
    }

    /// Start the TCP listener on `0.0.0.0:port` (no-op when `port <= 0` or the
    /// listener is already running). Each accepted connection is pinned to its
    /// own I/O thread.
    pub(crate) fn listen(self: &Arc<Self>, port: i64) {
        if port <= 0 {
            return;
        }
        if self.listening.swap(true, Ordering::AcqRel) {
            return;
        }
        let gateway = Arc::clone(self);
        thread::spawn(move || {
            let addr = format!("0.0.0.0:{}", port);
            let listener = match TcpListener::bind(&addr) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("[docker-host-gateway] bind {} failed: {}", addr, e);
                    gateway.listening.store(false, Ordering::Release);
                    return;
                }
            };
            eprintln!("[docker-host-gateway] listening on {}", addr);
            // Reaper: keep the cold-spawn signal queue bounded even when a
            // creature never reconnects, so undeliverable packets cannot pin node
            // memory forever.
            let reaper = Arc::clone(&gateway);
            thread::spawn(move || loop {
                thread::sleep(std::time::Duration::from_secs(30));
                reaper.registry.sweep_expired();
            });
            for incoming in listener.incoming() {
                match incoming {
                    Ok(stream) => {
                        let g = Arc::clone(&gateway);
                        thread::spawn(move || run_connection(g, stream));
                    }
                    Err(e) => {
                        // Never abandon the listener over a single accept error.
                        eprintln!("[docker-host-gateway] accept error: {}", e);
                        continue;
                    }
                }
            }
        });
    }

    /// Push a signal to every live container belonging to `machine_id`. Returns
    /// the number of containers reached (`0` ⇒ none connected).
    pub(crate) fn push_signal_to_machine(
        &self,
        machine_id: &str,
        key: &str,
        data: &JsonValue,
    ) -> usize {
        self.registry.push_signal_to_machine(machine_id, key, data)
    }

    /// Push a signal to the container serving a specific entity of a machine.
    /// Returns the number reached (`0` ⇒ cold, so the caller queues/spawns it).
    pub(crate) fn push_signal_to_entity(
        &self,
        machine_id: &str,
        entity_id: &str,
        key: &str,
        data: &JsonValue,
    ) -> usize {
        self.registry
            .push_signal_to_entity(machine_id, entity_id, key, data)
    }

    /// Queue a signal for an entity that has no live connection yet, to be
    /// flushed when its container (re)connects. See [`GatewayRegistry::queue_pending_signal`].
    pub(crate) fn queue_pending_signal(
        &self,
        machine_id: &str,
        entity_id: &str,
        key: &str,
        data: &JsonValue,
    ) {
        self.registry
            .queue_pending_signal(machine_id, entity_id, key, data);
    }

    /// Claim the cold-spawn slot for an entity (debounce). `true` ⇒ this caller
    /// should boot the container; `false` ⇒ a spawn is already in flight.
    pub(crate) fn begin_cold_spawn(&self, machine_id: &str, entity_id: &str) -> bool {
        self.registry.begin_cold_spawn(machine_id, entity_id)
    }

    /// Push a signal to the specific container identified by `vm_id`.
    pub(crate) fn push_signal_to_vm(&self, vm_id: &str, key: &str, data: &JsonValue) -> bool {
        match self.registry.by_vm(vm_id) {
            Some(conn) => {
                conn.push_signal(key, data);
                true
            }
            None => false,
        }
    }

    /// Number of currently-connected containers.
    pub(crate) fn live_connection_count(&self) -> usize {
        self.registry.count()
    }
}
