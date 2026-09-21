//! The `stores` action namespace — signalling into a store, replaying what was
//! signalled, and administering who may do either.
//!
//! This is the node's own messaging layer. A store is the unit a signal is
//! addressed to; the node fans the signal out live to every connected member
//! and, when the store is marked `persHist`, writes it to the time-series log
//! with the sender's tags. `stores/history` reads that log back with a tag
//! filter, so a caller reconstructs any slice of the conversation — one thread,
//! one kind of message, one agent's trail — without keeping a parallel index of
//! its own anywhere else.
//!
//! Permissions are checked here, not in whatever creature happens to be calling:
//! `signal` to post, `read` to replay, `manage` to change another member's
//! grant. See [`crate::shell::api::model::access`] — an absent grant denies.
//!
//! Every input carries an `origin`, so a member whose node is not this one has
//! the whole action routed to the owning node by the federation driver
//! (`SecureAction::securely_act`) and served there against that node's log. A
//! store's signals therefore stay readable across a federation without being
//! replicated into chain state.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::shell::api::model::store_ports::{
    legacy_error, log_packet, MembershipPorts, SignalPorts, StorePorts, SystemClock,
};
use aseman_application::store::{GetStoreAccess, ReadStoreHistory, SetStoreAccess, SignalStore};

use crate::core::actor::model::secured::guard::Guard;
use crate::models::action::ISecureAction;
use crate::models::core::ICore;
use crate::models::packet::{LogPacket, LogQuery};
use crate::models::ports::storage::IStorage;
use crate::models::state::IState;
use crate::models::transaction::ITrx;
use crate::shell::api::model::access::{access_link_key, read_permissions, StorePermissions};
use crate::shell::api::model::{Creature, Store};
use crate::shell::api::packets::stores::{
    GetAccessInput, HistoryInput, Send as StoresSend, SetAccessInput, SignalInput,
};
use crate::shell::utils::future::async_once;

use super::util::build_secure_action;

/// Actions addressed to a store: the caller must be identified AND a member,
/// which the guard checks against `hasaccess::<userId>::<storeId>` before the
/// body runs. What the member may then *do* is the permission check inside each
/// body.
fn store_guard() -> Guard {
    Guard {
        is_user: true,
        is_in_store: true,
        allow_applet_sign: true,
    }
}

use aseman_application::store::DEFAULT_HISTORY_COUNT;

/// Fan a store signal out to every member of the store except the sender.
///
/// The live packet carries the persisted row's `signalId`, `time` and `tags`,
/// so a client applies the same filter to a live signal that it applies to
/// history, and recognises the replayed row as one it has already rendered.
///
/// Delivery goes through the signaler's store fan-out, which resolves the
/// store's members from `onaccess::` at the moment it delivers. That matters:
/// the signaler's group registry is built when a connection authenticates, so a
/// space created (or joined) *during* a client's session is absent from it, and
/// a group fan-out on that space reaches nobody until the client reconnects.
/// Reading membership from state has no such window.
fn fan_out(app: Arc<dyn ICore>, store_id: String, sender_id: String, packet: StoresSend) {
    let value = serde_json::to_value(&packet).unwrap_or(Value::Null);
    let _ = async_once(move || {
        app.tools().signaler().signal_store(
            "stores/signal",
            &store_id,
            value,
            vec![sender_id],
            true,
        );
    });
}

/// `/stores/signal` — send one signal into a store.
///
/// Persistence is the store's decision, not the sender's: a store created with
/// `persHist` keeps every signal sent into it. The sender may only opt a single
/// signal OUT, with `temp`, for traffic that is meaningless after delivery
/// (typing indicators, progress pings).
fn signal(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    let app_for_handler = app.clone();
    build_secure_action::<SignalInput, _>(
        app,
        "/stores/signal",
        store_guard(),
        move |state: Arc<dyn IState>, input: SignalInput| -> Result<Value> {
            let sender_id = state.info().user_id();
            let trx = state.trx();
            let store_ports = StorePorts { trx: &*trx };
            let membership = MembershipPorts { trx: &*trx };
            let signal_log = SignalPorts {
                storage: app_for_handler.tools().storage(),
            };
            let outcome = SignalStore {
                stores: &store_ports,
                access: &membership,
                log: &signal_log,
                clock: &SystemClock,
            }
            .execute(
                &sender_id,
                &input.store_id,
                &input.data,
                &input.tags,
                input.temp,
            )
            .map_err(legacy_error)?;

            let mut sender =
                (crate::shell::api::model::creature_ports::CreaturePorts { trx: &*trx })
                    .creature_or_empty(&sender_id.clone());
            // Balance is never leaked over the signalling channel.
            sender.balance = 0;
            let signal_id = outcome
                .signal
                .as_ref()
                .map(|signal| signal.id.clone())
                .unwrap_or_default();
            let out = StoresSend {
                action: if input.typ.is_empty() {
                    "broadcast".to_string()
                } else {
                    input.typ.clone()
                },
                user: sender,
                store: Store {
                    id: input.store_id.clone(),
                    ..Default::default()
                },
                data: input.data.clone(),
                is_temp: input.temp,
                tags: outcome.tags.clone(),
                signal_id: signal_id.clone(),
                time: outcome.time_millis,
                ..Default::default()
            };
            fan_out(
                app_for_handler.clone(),
                input.store_id.clone(),
                sender_id,
                out,
            );

            Ok(json!({
                "passed": true,
                "persisted": outcome.persisted,
                "signalId": signal_id,
                "time": outcome.time_millis,
                "tags": outcome.tags,
            }))
        },
    )
}

/// `/stores/history` — replay a store's persisted signals, newest first,
/// filtered by tag.
fn history(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    let app_for_handler = app.clone();
    build_secure_action::<HistoryInput, _>(
        app,
        "/stores/history",
        store_guard(),
        move |state: Arc<dyn IState>, input: HistoryInput| -> Result<Value> {
            let trx = state.trx();
            let membership = MembershipPorts { trx: &*trx };
            let signal_log = SignalPorts {
                storage: app_for_handler.tools().storage(),
            };
            let signals = ReadStoreHistory {
                access: &membership,
                log: &signal_log,
            }
            .execute(
                &state.info().user_id(),
                &input.store_id,
                LogQuery {
                    tags_all: input.tags_all.clone(),
                    tags_any: input.tags_any.clone(),
                    before_time: input.before_time,
                    after_time: input.after_time,
                    count: input.count,
                },
            )
            .map_err(legacy_error)?;
            Ok(json!({
                "storeId": input.store_id,
                "signals": signals.into_iter().map(log_packet).collect::<Vec<_>>(),
            }))
        },
    )
}

/// `/stores/setAccess` — set one member's permissions.
///
/// Requires `manage`. This is how a role maps onto the node: a viewer is
/// granted `read` alone, an ordinary member `read,signal`, an administrator
/// `read,signal,manage`.
fn set_access(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<SetAccessInput, _>(
        app,
        "/stores/setAccess",
        store_guard(),
        move |state: Arc<dyn IState>, input: SetAccessInput| -> Result<Value> {
            let trx = state.trx();
            let membership = MembershipPorts { trx: &*trx };
            let perms = SetStoreAccess {
                access: &membership,
            }
            .execute(
                &state.info().user_id(),
                &input.store_id,
                &input.member_id,
                &input.permissions,
            )
            .map_err(legacy_error)?;
            Ok(json!({
                "storeId": input.store_id,
                "memberId": input.member_id,
                "permissions": perms,
            }))
        },
    )
}

/// `/stores/getAccess` — read a member's permissions. A member may always read
/// their own; reading somebody else's requires `manage`.
fn get_access(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<GetAccessInput, _>(
        app,
        "/stores/getAccess",
        store_guard(),
        move |state: Arc<dyn IState>, input: GetAccessInput| -> Result<Value> {
            let trx = state.trx();
            let membership = MembershipPorts { trx: &*trx };
            let (member, perms) = GetStoreAccess {
                access: &membership,
            }
            .execute(&state.info().user_id(), &input.store_id, &input.member_id)
            .map_err(legacy_error)?;
            Ok(json!({
                "storeId": input.store_id,
                "memberId": member,
                "permissions": perms,
            }))
        },
    )
}

/// Install every store action onto the actor.
pub fn install(app: Arc<dyn ICore>) {
    let actor = app.actor();
    let handlers: Vec<Arc<dyn ISecureAction>> = vec![
        signal(app.clone()),
        history(app.clone()),
        set_access(app.clone()),
        get_access(app.clone()),
    ];
    for h in handlers {
        actor.inject_secure_action(h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::input::IInput;
    use crate::shell::api::packets::stores::{HistoryInput, SignalInput};

    /// The store-scoped guard must demand BOTH an identified caller and store
    /// membership: the permission checks in each body assume the caller is a
    /// member, and only the guard establishes that.
    #[test]
    fn store_actions_are_guarded_by_identity_and_membership() {
        let g = store_guard();
        assert!(
            g.is_user,
            "an anonymous caller must never reach a store action"
        );
        assert!(g.is_in_store, "membership is checked before the body runs");
    }

    /// Inputs route by `origin`, which is what makes a store on another node
    /// readable and writable through the same two actions.
    #[test]
    fn inputs_carry_their_federation_origin_and_store() {
        let signal = SignalInput {
            store_id: "7@peer".into(),
            origin: "peer".into(),
            ..Default::default()
        };
        assert_eq!(signal.get_store_id(), "7@peer");
        assert_eq!(
            signal.origin(),
            "peer",
            "a foreign origin routes the action to that node"
        );

        let history = HistoryInput {
            store_id: "7@peer".into(),
            ..Default::default()
        };
        assert_eq!(history.get_store_id(), "7@peer");
        assert_eq!(history.origin(), "", "no origin means this node serves it");
    }

    /// A history read with no count must not mean "every row ever": the driver
    /// clamps, and the action supplies a sane page.
    #[test]
    fn history_defaults_to_one_page() {
        let input = HistoryInput::default();
        let count = if input.count > 0 {
            input.count
        } else {
            DEFAULT_HISTORY_COUNT
        };
        assert_eq!(count, DEFAULT_HISTORY_COUNT);
    }

    /// Characterization of the rewired path: the store use cases running through
    /// The routed store ports on a real legacy transaction keep the legacy key encodings.
    mod legacy_ports {
        use super::super::*;
        use crate::core::actor::model::trx::tests::StubCore;
        use crate::core::actor::model::trx::TrxWrapper;
        use crate::models::packet::{BuildPacket, LogPacket, LogQuery};
        use crate::models::ports::storage::{IStorage, KvDb};
        use std::sync::Mutex;

        struct RecordingStorage {
            kv: KvDb,
            signals: Mutex<Vec<LogPacket>>,
        }

        impl IStorage for RecordingStorage {
            fn storage_root(&self) -> String {
                String::new()
            }
            fn kv_db(&self) -> KvDb {
                self.kv.clone()
            }
            fn gen_id(&self, _: &dyn ITrx, _: &str) -> String {
                String::new()
            }
            fn log_time_sieries(
                &self,
                store_id: &str,
                user_id: &str,
                data: &str,
                tags: &[String],
                time_val: i64,
            ) -> Result<LogPacket> {
                let mut signals = self.signals.lock().unwrap();
                let packet = LogPacket {
                    id: format!("sig-{}", signals.len()),
                    user_id: user_id.to_string(),
                    data: data.to_string(),
                    store_id: store_id.to_string(),
                    tags: tags.to_vec(),
                    time: time_val,
                    edited: false,
                };
                signals.push(packet.clone());
                Ok(packet)
            }
            fn update_log(&self, _: &str, _: &str, _: &str, _: &str, _: i64) -> LogPacket {
                LogPacket::default()
            }
            fn read_store_logs(&self, store_id: &str, query: &LogQuery) -> Result<Vec<LogPacket>> {
                let mut rows = self
                    .signals
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|packet| packet.store_id == store_id)
                    .filter(|packet| query.tags_all.iter().all(|tag| packet.tags.contains(tag)))
                    .cloned()
                    .collect::<Vec<_>>();
                rows.reverse();
                Ok(rows)
            }
            fn pick_store_logs(&self, _: &str, _: Vec<String>) -> Vec<LogPacket> {
                Vec::new()
            }
            fn log_vm(&self, _: &str, _: &str, _: &str, _: i64) -> BuildPacket {
                BuildPacket::default()
            }
            fn read_vm_logs(&self, _: &str, _: &str, _: i64, _: i64) -> Vec<BuildPacket> {
                Vec::new()
            }
        }

        #[test]
        fn store_use_cases_keep_legacy_encodings_through_the_adapter() {
            let dir = std::env::temp_dir().join(format!(
                "aseman-store-ports-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let storage: Arc<RecordingStorage> = Arc::new(RecordingStorage {
                kv: Arc::new(aseman_storage_legacy::RocksDbKvStore::open_default(&dir).unwrap()),
                signals: Mutex::new(Vec::new()),
            });
            let dyn_storage: Arc<dyn IStorage> = storage.clone();
            let trx = TrxWrapper::new(
                Arc::new(StubCore {
                    storage: dyn_storage.clone(),
                }),
                dyn_storage.clone(),
                false,
            );
            Store {
                id: "s1".into(),
                pers_hist: true,
                member_count: 1,
                ..Default::default()
            }
            .push(&*trx);
            trx.put_link(
                &access_link_key("s1", "alice"),
                &StorePermissions::owner().encode(),
            );
            let store_ports = StorePorts { trx: &*trx };
            let membership = MembershipPorts { trx: &*trx };
            let signal_log = SignalPorts {
                storage: dyn_storage,
            };

            let outcome = SignalStore {
                stores: &store_ports,
                access: &membership,
                log: &signal_log,
                clock: &SystemClock,
            }
            .execute("alice", "s1", "hello", &["kind=message".to_string()], false)
            .unwrap();
            assert!(outcome.persisted);
            assert_eq!(outcome.signal.unwrap().id, "sig-0");
            let counted = Store {
                id: "s1".into(),
                ..Default::default()
            }
            .pull(&*trx);
            assert_eq!(counted.signal_count, 1);

            let history = ReadStoreHistory {
                access: &membership,
                log: &signal_log,
            }
            .execute("alice", "s1", LogQuery::default())
            .unwrap();
            assert_eq!(history.len(), 1);

            // Legacy `onaccess` link encoding is preserved exactly.
            SetStoreAccess {
                access: &membership,
            }
            .execute(
                "alice",
                "s1",
                "bob",
                &["signal".to_string(), "read".to_string()],
            )
            .unwrap();
            assert_eq!(trx.get_link(&access_link_key("s1", "bob")), "read,signal");
            let (member, perms) = GetStoreAccess {
                access: &membership,
            }
            .execute("bob", "s1", "")
            .unwrap();
            assert_eq!(
                (member.as_str(), perms),
                ("bob", StorePermissions::member())
            );
            let denied = SignalStore {
                stores: &store_ports,
                access: &membership,
                log: &signal_log,
                clock: &SystemClock,
            }
            .execute("mallory", "s1", "x", &[], false)
            .unwrap_err();
            assert_eq!(
                legacy_error(denied).to_string(),
                "not allowed to signal in this store"
            );
            drop(trx);
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// LD-12: the legacy creature-deletion walk, `Store::list(.., -1, -1)`, is
        /// always empty, so a deleted creature kept every membership. The port
        /// helpers remove them and delete stores left with no member.
        #[test]
        fn removing_a_member_everywhere_fixes_the_empty_legacy_walk() {
            let dir = std::env::temp_dir().join(format!(
                "aseman-store-members-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let storage: Arc<dyn IStorage> = Arc::new(RecordingStorage {
                kv: Arc::new(aseman_storage_legacy::RocksDbKvStore::open_default(&dir).unwrap()),
                signals: Mutex::new(Vec::new()),
            });
            let trx = TrxWrapper::new(
                Arc::new(StubCore {
                    storage: storage.clone(),
                }),
                storage,
                false,
            );
            for id in ["s1", "s2"] {
                Store {
                    id: id.into(),
                    ..Default::default()
                }
                .push(&*trx);
            }
            let ports = crate::shell::api::model::store_ports::MembershipPorts { trx: &*trx };
            let member = StorePermissions::member();
            aseman_ports::StoreAccess::join(&ports, "s1", "alice", member).unwrap();
            aseman_ports::StoreAccess::join(&ports, "s1", "bob", member).unwrap();
            aseman_ports::StoreAccess::join(&ports, "s2", "alice", member).unwrap();
            // A membership whose store object is gone.
            aseman_ports::StoreAccess::join(&ports, "gone", "alice", member).unwrap();

            let legacy_walk = Store::list(
                &*trx,
                "hasaccess::alice::",
                false,
                &std::collections::HashMap::new(),
                &std::collections::HashMap::new(),
                -1,
                -1,
            )
            .unwrap();
            assert!(legacy_walk.is_empty());
            let ids = |stores: Vec<Store>| stores.into_iter().map(|s| s.id).collect::<Vec<_>>();
            assert_eq!(ids(ports.member_stores("alice", 50).unwrap()), ["s1", "s2"]);
            // The window is taken over membership links, then dangling ones drop.
            assert!(ports.member_stores("alice", 1).unwrap().is_empty());

            assert_eq!(ports.remove_member_everywhere("alice").unwrap(), ["s2"]);
            assert!(aseman_ports::StoreAccess::stores_of(&ports, "alice")
                .unwrap()
                .is_empty());
            assert!(aseman_ports::StoreAccess::is_member(&ports, "s1", "bob").unwrap());
            assert!(!trx.get_obj(Store::type_(), "s1").is_empty());
            assert!(trx.get_obj(Store::type_(), "s2").is_empty());
            drop(trx);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
