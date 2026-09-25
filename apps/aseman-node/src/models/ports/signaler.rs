//! Signaler port — the realtime pub/sub fan-out driver.

use std::sync::Arc;

use dashmap::DashMap;
use serde_json::Value;

/// Callback invoked when a signal fires. Receives the signal key and payload.
pub type SignalFn = Arc<dyn Fn(String, Value) + Send + Sync>;

/// Callback invoked on group join/leave. Receives the group id and user id.
pub type JoinFn = Arc<dyn Fn(String, String) + Send + Sync>;

/// A group of stores sharing a single listener.
///
/// `listener` and `override_` are mutated after construction (see
/// `ISignaler::listen_to_group`), so they live behind a `Mutex` to stay
/// thread-safe.
pub struct Group {
    pub stores: Arc<DashMap<String, String>>,
    pub listener: std::sync::Mutex<Option<Arc<Listener>>>,
    pub override_: std::sync::Mutex<bool>,
}

/// A single signal listener.
#[derive(Clone)]
pub struct Listener {
    pub id: String,
    pub paused: bool,
    pub dis_time: i64,
    pub signal: SignalFn,
}

/// A listener that bridges every signal globally.
#[derive(Clone)]
pub struct GlobalListener {
    pub signal: SignalFn,
}

/// A listener for group join/leave events.
#[derive(Clone)]
pub struct JoinListener {
    pub join: JoinFn,
    pub leave: JoinFn,
}

/// The signaler driver interface — realtime pub/sub fan-out.
pub trait ISignaler: Send + Sync {
    fn lock(&self);
    fn unlock(&self);
    fn listeners(&self) -> Arc<DashMap<String, Arc<Listener>>>;
    fn groups(&self) -> Arc<DashMap<String, Arc<Group>>>;
    fn listen_to_single(&self, listener: Arc<Listener>);
    fn listen_to_group(&self, listener: Arc<Listener>, override_functionaly: bool);
    fn brdige_globally(&self, listener: Arc<GlobalListener>, override_functionaly: bool);
    fn listen_to_join(&self, listener: Arc<JoinListener>);
    fn signal_user(&self, key: &str, listener_id: &str, data: Value, pack: bool);
    fn signal_group(
        &self,
        key: &str,
        group_id: &str,
        data: Value,
        pack: bool,
        exceptions: Vec<String>,
    );
    /// Fan a signal out to every current member of a store.
    ///
    /// Membership is resolved from the state's `onaccess::<store>::<member>`
    /// grants at the moment of delivery — NOT from the in-memory group
    /// registry. The registry is populated when a connection authenticates (and
    /// when a program boots), so a store a member gained access to *after* that
    /// point is not in it, and a signal on that store would reach nobody until
    /// the member reconnected. A store's membership is state; reading it from
    /// state is the only way the fan-out cannot go stale.
    ///
    /// Only members whose grant carries `read` are delivered to — the same flag
    /// `stores/history` requires — so what a member is pushed live and what they
    /// may replay never diverge.
    ///
    /// `exceptions` are member ids to skip (the sender). `federate` pushes the
    /// same packet to each peer origin holding a member; pass `false` when
    /// re-emitting a packet that already arrived over federation, so it is not
    /// bounced back around the network.
    fn signal_store(
        &self,
        key: &str,
        store_id: &str,
        data: Value,
        exceptions: Vec<String>,
        federate: bool,
    );
    fn join_group(&self, group_id: &str, user_id: &str);
    fn leave_group(&self, group_id: &str, user_id: &str);
    /// Remove `user_id` from every group it is a member of, reaping any group
    /// left with no members, listener, or override. Called when a user's last
    /// connection closes so a disconnected client's group memberships (and the
    /// now-empty groups they leave behind) cannot accumulate for the life of
    /// the node.
    fn leave_all_groups(&self, user_id: &str);
    fn retrive_group(&self, group_id: &str) -> Option<Arc<Group>>;
}
