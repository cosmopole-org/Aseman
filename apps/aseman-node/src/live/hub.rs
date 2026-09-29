//! The session hub: pushes signals to connected users and machines, and to the
//! members of stores (resolved from their access grants when delivered), and
//! forwards to peer nodes the members that live there.

use std::sync::{Arc, Mutex};

use dashmap::DashMap;
use serde_json::Value;

use crate::node::Node;
use crate::transports::federation::FedNet;

/// Callback invoked when a signal fires. Receives the signal key and payload.
pub type SignalFn = Arc<dyn Fn(String, Value) + Send + Sync>;

/// Callback invoked on group join/leave. Receives the group id and user id.
pub type JoinFn = Arc<dyn Fn(String, String) + Send + Sync>;

/// A group of stores sharing a single listener.
///
/// `listener` and `override_` are mutated after construction (see
/// `Signaler::listen_to_group`), so they live behind a `Mutex` to stay
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

/// The session hub. Owns per-listener / per-group
/// `DashMap`s.
pub struct Signaler {
    lock: Mutex<()>,
    app: Arc<Node>,
    listeners: Arc<DashMap<String, Arc<Listener>>>,
    groups: Arc<DashMap<String, Arc<Group>>>,
    global_bridge: Mutex<Option<Arc<GlobalListener>>>,
    l_group_disabled: Mutex<bool>,
    j_listener: Mutex<Option<Arc<JoinListener>>>,
    federation: Arc<FedNet>,
}

impl Signaler {
    /// `NewSignaler(app, federation)`.
    pub fn new(app: Arc<Node>, federation: Arc<FedNet>) -> Arc<Signaler> {
        Arc::new(Signaler {
            lock: Mutex::new(()),
            app,
            listeners: Arc::new(DashMap::new()),
            groups: Arc::new(DashMap::new()),
            global_bridge: Mutex::new(None),
            l_group_disabled: Mutex::new(false),
            j_listener: Mutex::new(None),
            federation,
        })
    }

    /// Dispatch a signal to one in-process listener.
    fn signal_listener(&self, key: &str, listener_id: &str, data: Value) {
        let Some(listener) = self.listeners.get(listener_id).map(|e| e.value().clone()) else {
            return;
        };
        (listener.signal)(key.to_string(), data);
    }

    /// Drop a group from the registry once it has no members, no group
    /// listener, and no override — i.e. nothing that could ever be delivered
    /// to. `remove_if` evaluates the predicate under the shard write lock, so
    /// the emptiness check and the removal are atomic with respect to the
    /// `groups` map; a concurrent `join_group`/`listen_to_group` that
    /// re-populates the group either commits before the predicate (group is
    /// kept) or after the removal (re-inserts a fresh group), never losing a
    /// live membership without it self-healing on the next join.
    fn reap_group_if_empty(&self, group_id: &str) {
        self.groups.remove_if(group_id, |_, g| {
            g.stores.is_empty()
                && g.listener.lock().unwrap().is_none()
                && !*g.override_.lock().unwrap()
        });
    }

    /// The members of a store that may receive its signals, read from state.
    ///
    /// A store's membership lives in `onaccess::<store>::<member>`, whose value
    /// is the member's permission set. Only a member holding `read` is
    /// returned: that is the same flag `stores/history` demands, so a member is
    /// never pushed live what they could not replay.
    fn store_members(&self, store_id: &str) -> Vec<String> {
        self.app
            .read(|trx| {
                aseman_ports::StoreAccess::members(
                    &crate::state::store_ports::MembershipPorts { trx },
                    store_id,
                )
                .map_err(|error| anyhow::anyhow!("{error}"))
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, permissions)| permissions.read)
            .map(|(member, _)| member)
            .collect()
    }

    /// Read `User.<id>.username` inside a read-only state modification.
    fn read_user_username(&self, user_id: &str) -> String {
        self.app
            .read(|trx| {
                Ok(aseman_ports::CreatureDirectory::creature(
                    &crate::state::creature_ports::CreaturePorts { trx },
                    user_id,
                )
                .ok()
                .flatten()
                .map(|record| record.username)
                .unwrap_or_default())
            })
            .unwrap_or_default()
    }
}

/// Split a store's members into the ones this node delivers to itself and the
/// peer origins that have to be pushed to.
///
/// A member id is `<counter>@<origin>`. An origin that is this node's (or the
/// well-known `global`) is served from this node's own listener table; anything
/// else lives on a peer, which is pushed the packet once and skips the members
/// named in the exceptions it is handed. Excepted local members — the sender —
/// are simply dropped.
fn split_store_members(
    self_id: &str,
    members: Vec<String>,
    exceptions: &[String],
) -> (Vec<String>, std::collections::HashMap<String, Vec<String>>) {
    let exc: std::collections::HashSet<&str> = exceptions.iter().map(String::as_str).collect();
    let mut local: Vec<String> = Vec::new();
    let mut foreigners: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for member in members {
        let origin = member
            .rsplit_once('@')
            .map(|(_, o)| o.to_string())
            .unwrap_or_default();
        if origin.is_empty() || origin == self_id || origin == "global" {
            if !exc.contains(member.as_str()) {
                local.push(member);
            }
            continue;
        }
        let entry = foreigners.entry(origin).or_default();
        if exc.contains(member.as_str()) {
            entry.push(member);
        }
    }
    (local, foreigners)
}

impl Signaler {
    pub(crate) fn listeners(&self) -> Arc<DashMap<String, Arc<Listener>>> {
        self.listeners.clone()
    }

    #[cfg(test)]
    pub(crate) fn groups(&self) -> Arc<DashMap<String, Arc<Group>>> {
        self.groups.clone()
    }

    pub(crate) fn listen_to_single(&self, listener: Arc<Listener>) {
        let _g = self.lock.lock().unwrap();
        self.listeners.insert(listener.id.clone(), listener);
    }

    #[cfg(test)]
    pub(crate) fn listen_to_group(&self, listener: Arc<Listener>, override_functionaly: bool) {
        let group = self
            .retrive_group(&listener.id)
            .expect("retrive_group always returns Some");
        *group.listener.lock().unwrap() = Some(listener);
        *group.override_.lock().unwrap() = override_functionaly;
    }
    pub(crate) fn signal_user(&self, key: &str, listener_id: &str, data: Value) {
        if !listener_id.contains('@') {
            self.signal_listener(key, listener_id, data);
            return;
        }
        if self.listeners.contains_key(listener_id) {
            self.signal_listener(key, listener_id, data);
            return;
        }
        let username = self.read_user_username(listener_id);
        if username.is_empty() {
            // Nothing is listening on this id, and it is not a creature whose
            // origin could route it onward — so the target is a program that
            // should have registered a listener when it was deployed (and had it
            // restored at startup). Dropping that in silence is how a signal to a
            // stale or dead program id becomes "the agent just never answers":
            // no error to the sender, no trace on the node, nothing to grep.
            eprintln!(
                "[signal] dropped key={} target={} reason=no-listener-and-no-creature",
                key, listener_id
            );
            return;
        }
        let Some(origin) = username.rsplit_once('@').map(|(_, s)| s.to_string()) else {
            return;
        };
        if origin == self.app.id() {
            self.signal_listener(key, listener_id, data);
        } else {
            self.federation
                .send_fed_update(&origin, key, data, "user", listener_id, Vec::new());
        }
    }

    pub(crate) fn signal_group(
        &self,
        key: &str,
        group_id: &str,
        data: Value,
        exceptions: Vec<String>,
    ) {
        let packet = data.clone();

        // Global-bridge mode fans out through the bridge and never touches the
        // group map, so check it first — before any group lookup.
        if *self.l_group_disabled.lock().unwrap() {
            if let Some(bridge) = self.global_bridge.lock().unwrap().clone() {
                (bridge.signal)(group_id.to_string(), packet);
            }
            return;
        }

        // Get-only lookup: signalling a group nobody has joined and nothing
        // listens to must NOT materialise a permanent empty entry. The old
        // `retrive_group` here created (and never removed) one dead `Group`
        // per distinct `group_id` ever signalled — an unbounded leak on a node
        // that routes signals for transient stores. A group that truly has no
        // members and no listener has nothing to deliver to anyway.
        let Some(group) = self.groups.get(group_id).map(|e| e.value().clone()) else {
            return;
        };

        let exc: std::collections::HashSet<String> = exceptions.into_iter().collect();
        if *group.override_.lock().unwrap() {
            if let Some(listener) = group.listener.lock().unwrap().clone() {
                (listener.signal)(key.to_string(), packet);
            }
            return;
        }

        let mut foreigners: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        let store_entries: Vec<(String, String)> = group
            .stores
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();
        for (store_key, user_id) in store_entries {
            let username = self.read_user_username(&user_id);
            if username.is_empty() {
                continue;
            }
            let Some(user_origin) = username.rsplit_once('@').map(|(_, s)| s.to_string()) else {
                continue;
            };
            if user_origin == self.app.id() || user_origin == "global" {
                if !exc.contains(&store_key)
                    && let Some(listener) = self.listeners.get(&user_id).map(|e| e.value().clone())
                {
                    (listener.signal)(key.to_string(), packet.clone());
                }
            } else {
                let entry = foreigners.entry(user_origin).or_default();
                if exc.contains(&store_key) {
                    entry.push(user_id);
                }
            }
        }

        for (origin, exceptions_for_origin) in foreigners {
            self.federation.send_fed_update(
                &origin,
                key,
                data.clone(),
                "store",
                group_id,
                exceptions_for_origin,
            );
        }
    }

    pub(crate) fn signal_store(
        &self,
        key: &str,
        store_id: &str,
        data: Value,
        exceptions: Vec<String>,
        federate: bool,
    ) {
        // Global-bridge mode replaces per-member delivery entirely — the bridge
        // is the single sink for every signal — exactly as in `signal_group`.
        if *self.l_group_disabled.lock().unwrap() {
            if let Some(bridge) = self.global_bridge.lock().unwrap().clone() {
                (bridge.signal)(store_id.to_string(), data);
            }
            return;
        }

        let (local, foreigners) =
            split_store_members(&self.app.id(), self.store_members(store_id), &exceptions);

        for member in local {
            if let Some(listener) = self.listeners.get(&member).map(|e| e.value().clone()) {
                (listener.signal)(key.to_string(), data.clone());
            }
        }

        if !federate {
            return;
        }
        for (origin, exceptions_for_origin) in foreigners {
            self.federation.send_fed_update(
                &origin,
                key,
                data.clone(),
                "store",
                store_id,
                exceptions_for_origin,
            );
        }
    }

    pub(crate) fn join_group(&self, group_id: &str, user_id: &str) {
        let Some(g) = self.retrive_group(group_id) else {
            return;
        };
        g.stores.insert(user_id.to_string(), user_id.to_string());
        if let Some(j) = self.j_listener.lock().unwrap().clone() {
            (j.join)(group_id.to_string(), user_id.to_string());
        }
    }

    pub(crate) fn leave_group(&self, group_id: &str, user_id: &str) {
        // Get-only: never create a group just to leave it.
        let Some(g) = self.groups.get(group_id).map(|e| e.value().clone()) else {
            return;
        };
        g.stores.remove(user_id);
        if let Some(j) = self.j_listener.lock().unwrap().clone() {
            (j.leave)(group_id.to_string(), user_id.to_string());
        }
        self.reap_group_if_empty(group_id);
    }

    pub(crate) fn leave_all_groups(&self, user_id: &str) {
        // Snapshot the groups this user belongs to, then leave each. Collect
        // first so we never hold a `groups` shard read lock across the
        // `leave_group` writes/reaps (which take the shard write lock).
        let group_ids: Vec<String> = self
            .groups
            .iter()
            .filter(|e| e.value().stores.contains_key(user_id))
            .map(|e| e.key().clone())
            .collect();
        for group_id in group_ids {
            self.leave_group(&group_id, user_id);
        }
    }

    pub(crate) fn retrive_group(&self, group_id: &str) -> Option<Arc<Group>> {
        if let Some(existing) = self.groups.get(group_id) {
            return Some(existing.value().clone());
        }
        let fresh = Arc::new(Group {
            stores: Arc::new(DashMap::new()),
            listener: Mutex::new(None),
            override_: Mutex::new(false),
        });
        self.groups
            .entry(group_id.to_string())
            .or_insert(fresh)
            .value()
            .clone()
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_signaler() -> Arc<Signaler> {
        crate::node::Node::for_tests().tools().signaler()
    }

    fn noop_listener(id: &str) -> Arc<Listener> {
        let signal: SignalFn = Arc::new(|_k, _v| {});
        Arc::new(Listener {
            id: id.to_string(),
            signal,
        })
    }

    #[test]
    fn leave_group_reaps_the_group_once_its_last_member_goes() {
        let sig = new_signaler();
        sig.join_group("space1", "userA");
        sig.join_group("space1", "userB");
        assert_eq!(sig.groups().len(), 1);
        assert_eq!(sig.groups().get("space1").unwrap().stores.len(), 2);

        // One member leaving keeps the group alive for the survivor.
        sig.leave_group("space1", "userA");
        assert_eq!(sig.groups().len(), 1);
        assert_eq!(sig.groups().get("space1").unwrap().stores.len(), 1);

        // The last member leaving reaps the now-empty group entirely.
        sig.leave_group("space1", "userB");
        assert_eq!(sig.groups().len(), 0, "empty group must not linger");
    }

    #[test]
    fn leave_all_groups_clears_every_membership_and_reaps_empties() {
        let sig = new_signaler();
        sig.join_group("space1", "userA");
        sig.join_group("space1", "userB");
        sig.join_group("space2", "userA");
        assert_eq!(sig.groups().len(), 2);

        // A disconnecting user leaves all its groups at once; space2 (its only
        // member) is reaped, space1 survives for userB.
        sig.leave_all_groups("userA");
        let groups = sig.groups();
        assert!(
            groups.get("space2").is_none(),
            "single-member group must be reaped"
        );
        let s1 = groups.get("space1").unwrap();
        assert_eq!(s1.stores.len(), 1);
        assert!(s1.stores.contains_key("userB"));
    }

    /// A store fan-out must reach every LOCAL member except the sender, and
    /// must push each foreign origin exactly once, carrying that origin's own
    /// exceptions so the peer skips the same member this node would.
    #[test]
    fn store_fan_out_splits_local_members_from_peer_origins() {
        let members = vec![
            "1@global".to_string(),
            "2@global".to_string(),
            "3@peer-a".to_string(),
            "4@peer-a".to_string(),
            "5@peer-b".to_string(),
            "legacy".to_string(),
        ];
        let (local, foreign) = split_store_members(
            "global",
            members,
            &["2@global".to_string(), "4@peer-a".to_string()],
        );

        // The sender is dropped; an id with no origin is this node's own.
        assert_eq!(local, vec!["1@global".to_string(), "legacy".to_string()]);

        // Each peer is pushed once, whether or not it holds an excepted member.
        let mut origins: Vec<&String> = foreign.keys().collect();
        origins.sort();
        assert_eq!(origins, vec![&"peer-a".to_string(), &"peer-b".to_string()]);
        assert_eq!(foreign["peer-a"], vec!["4@peer-a".to_string()]);
        assert!(foreign["peer-b"].is_empty());
    }

    /// The whole point of resolving members from state: a store fan-out must
    /// not consult the group registry, so a store nobody has "joined" (because
    /// it was created after every current connection authenticated) still
    /// delivers — and signalling it never materialises a group.
    #[test]
    fn store_fan_out_needs_no_group_membership() {
        let sig = new_signaler();
        sig.signal_store(
            "stores/signal",
            "space-created-just-now",
            serde_json::json!({"n": 1}),
            Vec::new(),
            false,
        );
        assert_eq!(
            sig.groups().len(),
            0,
            "a store fan-out must not touch the group registry"
        );
    }

    #[test]
    fn signalling_an_unknown_group_never_materialises_a_group() {
        let sig = new_signaler();
        // No one has joined "ghost" and nothing listens to it. Before the fix
        // this created a permanent empty `Group` — the unbounded leak.
        sig.signal_group("k", "ghost", serde_json::json!({"n": 1}), Vec::new());
        assert_eq!(sig.groups().len(), 0, "signalling must not create groups");
    }

    #[test]
    fn a_group_with_a_listener_is_not_reaped_when_emptied() {
        let sig = new_signaler();
        // Machine/program groups carry a listener (via `listen_to_group`) and
        // legitimately have no store members — they must survive reaping.
        sig.listen_to_group(noop_listener("machine1"), true);
        sig.join_group("machine1", "userA");
        sig.leave_group("machine1", "userA");
        assert_eq!(sig.groups().len(), 1, "listener-backed group must be kept");
        assert!(
            sig.groups()
                .get("machine1")
                .unwrap()
                .listener
                .lock()
                .unwrap()
                .is_some()
        );
    }
}
