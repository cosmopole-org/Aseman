//! Per-connection state and the global registry of live container connections.
//!
//! Each docker creature container owns exactly one [`GatewayConnection`]. The
//! owning `TcpStream` lives inside that connection's dedicated I/O thread (see
//! [`super::server`]); everything reachable through `GatewayConnection` is safe
//! to call from any thread. Outbound frames are handed to the I/O thread over
//! an MPSC channel so signal pushes never block on socket writes and never
//! contend with the inbound read loop.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;

use dashmap::mapref::entry::Entry;

use crate::docker_host::prelude::*;
use crate::docker_host::protocol::{Opcode, encode_json_message, next_message_id};

/// How long a signal may sit in the cold-spawn queue before it is dropped. A
/// container that never comes back (a spawn that failed, a crash-looping image)
/// must not pin its callers' packets in node memory forever — they are swept
/// after this window and the caller falls back to its own timeout.
const PENDING_TTL: Duration = Duration::from_secs(90);
/// Hard cap on queued signals per entity, so a flood at a dead creature cannot
/// grow unbounded between sweeps. Oldest are dropped first.
const PENDING_CAP: usize = 256;
/// While a cold-spawn is in flight for an entity, further signals only queue —
/// they do not each trigger another spawn. The slot self-expires after this
/// window so a spawn that never connected can be retried.
const SPAWN_DEBOUNCE: Duration = Duration::from_secs(45);

/// A signal captured while its target entity had no live gateway connection,
/// held until the container (re)connects and can be handed the packet.
struct PendingSignal {
    key: String,
    data: JsonValue,
    at: Instant,
}

/// The `(machine_id, entity_id)` slot key for the per-entity pending queue and
/// spawn-debounce maps. `\x1f` (unit separator) cannot appear in an id, so the
/// join is unambiguous.
fn slot_key(machine_id: &str, entity_id: &str) -> String {
    format!("{machine_id}\u{1f}{entity_id}")
}

/// Frames the connection I/O thread accepts from any thread.
pub(crate) enum OutboundFrame {
    Body(Vec<u8>),
    Shutdown,
}

/// The identity a container declares during the `HELLO` handshake. Mirrors the
/// VM execution context the node already tracks (`register_vm_context`).
#[derive(Clone, Debug, Default)]
pub(crate) struct ContainerIdentity {
    pub(crate) vm_id: String,
    pub(crate) machine_id: String,
    pub(crate) creature_id: String,
    pub(crate) program_id: String,
    /// Which entity of the machine this container serves. A machine (program) can
    /// host several docker entities, each its own image and container, so signal
    /// delivery, the cold-spawn queue and the spawn debounce are all keyed by
    /// `(machine_id, entity_id)` — never the machine alone.
    pub(crate) entity_id: String,
}

/// A single live container connection.
pub(crate) struct GatewayConnection {
    pub(crate) conn_id: u64,
    pub(crate) identity: ContainerIdentity,
    disconnected: AtomicBool,
    outbound: Mutex<Option<Sender<OutboundFrame>>>,
}

impl GatewayConnection {
    pub(crate) fn new(
        conn_id: u64,
        identity: ContainerIdentity,
        outbound: Sender<OutboundFrame>,
    ) -> Arc<GatewayConnection> {
        Arc::new(GatewayConnection {
            conn_id,
            identity,
            disconnected: AtomicBool::new(false),
            outbound: Mutex::new(Some(outbound)),
        })
    }

    /// Queue an already-encoded frame for the I/O thread. Silent no-op once the
    /// connection has been torn down.
    pub(crate) fn enqueue(&self, frame: Vec<u8>) {
        if self.disconnected.load(Ordering::Acquire) {
            return;
        }
        if let Some(tx) = self.outbound.lock().unwrap().as_ref() {
            let _ = tx.send(OutboundFrame::Body(frame));
        }
    }

    /// Enqueue every frame of an encoded (possibly multi-chunk) message.
    pub(crate) fn enqueue_all(&self, frames: Vec<Vec<u8>>) {
        for frame in frames {
            self.enqueue(frame);
        }
    }

    /// Push a signal/notification to the container. The payload is delivered to
    /// the container's signal handler as `{key, data}`.
    pub(crate) fn push_signal(&self, key: &str, data: &JsonValue) {
        let envelope = json!({ "key": key, "data": data });
        let frames = encode_json_message(Opcode::Signal, next_message_id(), 0, &envelope);
        self.enqueue_all(frames);
    }

    /// Idempotent cooperative shutdown — wakes the I/O thread so it can flush
    /// and close.
    pub(crate) fn shutdown(&self) {
        if self.disconnected.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(tx) = self.outbound.lock().unwrap().take() {
            let _ = tx.send(OutboundFrame::Shutdown);
        }
    }

    pub(crate) fn is_disconnected(&self) -> bool {
        self.disconnected.load(Ordering::Acquire)
    }
}

/// Registry of live container connections, owned by a [`DockerHostGateway`]
/// instance (never a process-wide static — the gateway is reached through the
/// `ICore → tools() → vmm()` object graph).
///
/// Lookups iterate the (small) live-connection set rather than maintaining
/// secondary indexes — there is one connection per running docker VM, so the
/// linear scan is cheap and keeps the registry free of index-consistency bugs.
///
/// [`DockerHostGateway`]: crate::docker_host::gateway::DockerHostGateway
pub(crate) struct GatewayRegistry {
    conns: DashMap<u64, Arc<GatewayConnection>>,
    next_id: AtomicU64,
    /// Signals captured while an entity had no live connection, keyed by
    /// `(machine_id, entity_id)` (see `slot_key`) and flushed in FIFO order the
    /// moment that entity's container connects.
    pending: DashMap<String, VecDeque<PendingSignal>>,
    /// Entities with a cold-spawn currently in flight (debounce), keyed by
    /// `(machine_id, entity_id)`, so concurrent signals boot each entity once —
    /// and a sibling entity of the same machine is spawned independently.
    spawning: DashMap<String, Instant>,
}

impl GatewayRegistry {
    pub(crate) fn new() -> Self {
        Self {
            conns: DashMap::new(),
            next_id: AtomicU64::new(1),
            pending: DashMap::new(),
            spawning: DashMap::new(),
        }
    }

    pub(crate) fn alloc_conn_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn insert(&self, conn: Arc<GatewayConnection>) {
        // A re-connecting VM (same vm_id) supersedes any stale connection so
        // signals are never delivered to a dead socket.
        let vm_id = conn.identity.vm_id.clone();
        if !vm_id.is_empty() {
            let stale: Vec<u64> = self
                .conns
                .iter()
                .filter(|e| e.value().identity.vm_id == vm_id && e.value().conn_id != conn.conn_id)
                .map(|e| *e.key())
                .collect();
            for id in stale {
                if let Some((_, old)) = self.conns.remove(&id) {
                    old.shutdown();
                }
            }
        }
        self.conns.insert(conn.conn_id, conn);
    }

    pub(crate) fn remove(&self, conn_id: u64) {
        self.conns.remove(&conn_id);
    }

    pub(crate) fn count(&self) -> usize {
        self.conns.len()
    }

    /// Live connections serving a specific entity of a machine.
    fn by_entity(&self, machine_id: &str, entity_id: &str) -> Vec<Arc<GatewayConnection>> {
        self.conns
            .iter()
            .filter(|e| {
                let id = &e.value().identity;
                id.machine_id == machine_id
                    && id.entity_id == entity_id
                    && !e.value().is_disconnected()
            })
            .map(|e| e.value().clone())
            .collect()
    }

    /// Push a signal to the container(s) serving `entity_id` on `machine_id`.
    /// Returns the number reached (`0` ⇒ that entity has no live container, so the
    /// caller cold-spawns/queues it).
    pub(crate) fn push_signal_to_entity(
        &self,
        machine_id: &str,
        entity_id: &str,
        key: &str,
        data: &JsonValue,
    ) -> usize {
        let conns = self.by_entity(machine_id, entity_id);
        for conn in &conns {
            conn.push_signal(key, data);
        }
        conns.len()
    }

    /// The single newest live connection for an entity, or `None`. Cold-spawn
    /// flushes target exactly one container so a queued invoke is never delivered
    /// (and so executed) twice when a stale connection briefly overlaps a fresh
    /// one during a reconnect.
    fn newest_conn_for(&self, machine_id: &str, entity_id: &str) -> Option<Arc<GatewayConnection>> {
        self.by_entity(machine_id, entity_id)
            .into_iter()
            .max_by_key(|c| c.conn_id)
    }

    /// Capture a signal for an entity that has no live connection yet, so it can
    /// be delivered once its container (re)connects. Bounded by `PENDING_CAP`
    /// (oldest dropped) and pruned of anything past `PENDING_TTL`. If a connection
    /// has appeared since the caller last checked, the queue is flushed at once so
    /// the packet is never stranded by that race.
    pub(crate) fn queue_pending_signal(
        &self,
        machine_id: &str,
        entity_id: &str,
        key: &str,
        data: &JsonValue,
    ) {
        let slot = slot_key(machine_id, entity_id);
        {
            let mut q = self.pending.entry(slot).or_default();
            while q.front().is_some_and(|s| s.at.elapsed() > PENDING_TTL) {
                q.pop_front();
            }
            while q.len() >= PENDING_CAP {
                q.pop_front();
            }
            q.push_back(PendingSignal {
                key: key.to_string(),
                data: data.clone(),
                at: Instant::now(),
            });
        }
        if self.newest_conn_for(machine_id, entity_id).is_some() {
            self.flush_pending_signals(machine_id, entity_id);
        }
    }

    /// Deliver every queued signal for an entity to its live container, in the
    /// FIFO order they arrived, and clear the queue. The drain is atomic (the
    /// whole deque is removed at once), so a flush racing another — the connect
    /// path and the `queue_pending_signal` re-check firing together — can never
    /// deliver the same packet twice. Returns the number delivered. An entity with
    /// no live connection keeps its queue for the next connect.
    pub(crate) fn flush_pending_signals(&self, machine_id: &str, entity_id: &str) -> usize {
        let Some(conn) = self.newest_conn_for(machine_id, entity_id) else {
            return 0;
        };
        let Some((_, queued)) = self.pending.remove(&slot_key(machine_id, entity_id)) else {
            return 0;
        };
        let mut delivered = 0;
        for sig in queued {
            if sig.at.elapsed() > PENDING_TTL {
                continue; // stale: the caller has long since given up
            }
            conn.push_signal(&sig.key, &sig.data);
            delivered += 1;
        }
        delivered
    }

    /// Claim the cold-spawn slot for an entity. Returns `true` for the caller that
    /// should boot the container and `false` while a spawn started within
    /// `SPAWN_DEBOUNCE` is still in flight — the loser only queues its signal,
    /// which the flush on connect will deliver. The slot self-expires so a spawn
    /// that never connected can be retried later. Keyed per entity, so two entities
    /// of the same machine are booted independently.
    pub(crate) fn begin_cold_spawn(&self, machine_id: &str, entity_id: &str) -> bool {
        let now = Instant::now();
        match self.spawning.entry(slot_key(machine_id, entity_id)) {
            Entry::Occupied(mut e) => {
                if now.duration_since(*e.get()) < SPAWN_DEBOUNCE {
                    false
                } else {
                    e.insert(now);
                    true
                }
            }
            Entry::Vacant(e) => {
                e.insert(now);
                true
            }
        }
    }

    /// Release the cold-spawn slot — the container connected, so the next cold
    /// period may spawn that entity again immediately.
    pub(crate) fn clear_cold_spawn(&self, machine_id: &str, entity_id: &str) {
        self.spawning.remove(&slot_key(machine_id, entity_id));
    }

    /// Drop everything past its TTL so a creature that never comes back cannot pin
    /// queued packets (or a stale spawn slot) in memory forever.
    pub(crate) fn sweep_expired(&self) {
        self.pending.retain(|_, q| {
            q.retain(|s| s.at.elapsed() <= PENDING_TTL);
            !q.is_empty()
        });
        self.spawning.retain(|_, t| t.elapsed() < SPAWN_DEBOUNCE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cold_spawn_debounce_is_per_entity_not_per_machine() {
        let reg = GatewayRegistry::new();
        // First signal for entity A on machine M claims the spawn slot.
        assert!(reg.begin_cold_spawn("M", "A"));
        // A second concurrent signal for the same entity must NOT spawn again.
        assert!(!reg.begin_cold_spawn("M", "A"));
        // A different entity B on the SAME machine spawns independently — the
        // whole point: one entity being in flight must not starve its sibling.
        assert!(reg.begin_cold_spawn("M", "B"));
        assert!(!reg.begin_cold_spawn("M", "B"));
        // Releasing A's slot lets A spawn again, without touching B.
        reg.clear_cold_spawn("M", "A");
        assert!(reg.begin_cold_spawn("M", "A"));
        assert!(!reg.begin_cold_spawn("M", "B"));
    }

    #[test]
    fn queued_signals_for_a_cold_entity_survive_until_swept() {
        let reg = GatewayRegistry::new();
        // No live connection for this entity, so the signal is retained.
        reg.queue_pending_signal("M", "A", "creatures/signal", &json!({"n": 1}));
        reg.queue_pending_signal("M", "A", "creatures/signal", &json!({"n": 2}));
        assert_eq!(
            reg.pending.get(&slot_key("M", "A")).map(|q| q.len()),
            Some(2)
        );
        // A sibling entity keeps its own queue, never mixed with A's.
        reg.queue_pending_signal("M", "B", "creatures/signal", &json!({"n": 3}));
        assert_eq!(
            reg.pending.get(&slot_key("M", "B")).map(|q| q.len()),
            Some(1)
        );
        // A sweep with nothing expired keeps them (fresh); nothing is lost.
        reg.sweep_expired();
        assert_eq!(
            reg.pending.get(&slot_key("M", "A")).map(|q| q.len()),
            Some(2)
        );
    }
}
