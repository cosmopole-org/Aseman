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
//! grant. See [`crate::api::model::access`] — an absent grant denies.
//!
//! Every input carries an `origin`, so a member whose node is not this one has
//! the whole action routed to the owning node by the federation driver
//! (`SecureAction::securely_act`) and served there against that node's log. A
//! store's signals therefore stay readable across a federation without being
//! replicated into chain state.

use std::sync::Arc;

use anyhow::Result;
use serde_json::{Value, json};

use crate::api::model::store_ports::{
    MembershipPorts, SignalPorts, StorePorts, SystemClock, legacy_error, log_packet,
};
use aseman_application::store::{GetStoreAccess, ReadStoreHistory, SetStoreAccess, SignalStore};

use crate::api::model::Store;
use crate::api::packets::stores::{
    GetAccessInput, HistoryInput, Send as StoresSend, SetAccessInput, SignalInput,
};
use crate::api::utils::future::async_once;
use crate::core::actor::Guard;
use crate::models::action::ISecureAction;
use crate::models::core::ICore;
use crate::models::packet::LogQuery;
use crate::models::state::IState;

use super::util::build_secure_action;

#[cfg(test)]
use crate::api::model::access::StorePermissions;
#[cfg(test)]
use aseman_application::store::DEFAULT_HISTORY_COUNT;

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
            let store_ports = StorePorts { trx: &trx };
            let membership = MembershipPorts { trx: &trx };
            let signal_log = SignalPorts { trx: &trx };
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

            let mut sender = (crate::api::model::creature_ports::CreaturePorts { trx: &trx })
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
    build_secure_action::<HistoryInput, _>(
        app,
        "/stores/history",
        store_guard(),
        move |state: Arc<dyn IState>, input: HistoryInput| -> Result<Value> {
            let trx = state.trx();
            let membership = MembershipPorts { trx: &trx };
            let signal_log = SignalPorts { trx: &trx };
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
            let membership = MembershipPorts { trx: &trx };
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
            let membership = MembershipPorts { trx: &trx };
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
    use crate::api::packets::stores::{HistoryInput, SignalInput};
    use crate::models::input::IInput;

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

    /// The store use cases through the store ports on a storage transaction.
    mod ports {
        use super::super::*;
        use aseman_domain::store::StoreRecord;
        use aseman_ports::{StoreAccess, StoreDirectory};

        fn store(id: &str) -> StoreRecord {
            StoreRecord {
                id: id.into(),
                persistent_history: true,
                member_count: 1,
                ..Default::default()
            }
        }

        #[test]
        fn store_use_cases_run_through_the_ports() {
            let trx = crate::core::trx::test_trx();
            crate::api::model::conformance::seed_humans(&trx, &["1@t", "2@t", "3@t"]);
            let store_ports = StorePorts { trx: &trx };
            let membership = MembershipPorts { trx: &trx };
            let signal_log = SignalPorts { trx: &trx };
            store_ports.create_store(&store("s1"), "1@t").unwrap();
            membership
                .join("s1", "1@t", StorePermissions::owner())
                .unwrap();

            let outcome = SignalStore {
                stores: &store_ports,
                access: &membership,
                log: &signal_log,
                clock: &SystemClock,
            }
            .execute("1@t", "s1", "hello", &["kind=message".to_string()], false)
            .unwrap();
            assert!(outcome.persisted);
            let signal = outcome.signal.unwrap();
            assert!(!signal.id.is_empty());
            assert_eq!(store_ports.store_or_empty("s1").signal_count, 1);

            let history = ReadStoreHistory {
                access: &membership,
                log: &signal_log,
            }
            .execute("1@t", "s1", LogQuery::default())
            .unwrap();
            assert_eq!(
                history
                    .iter()
                    .map(|packet| packet.id.clone())
                    .collect::<Vec<_>>(),
                [signal.id]
            );

            SetStoreAccess {
                access: &membership,
            }
            .execute(
                "1@t",
                "s1",
                "2@t",
                &["signal".to_string(), "read".to_string()],
            )
            .unwrap();
            let (member, perms) = GetStoreAccess {
                access: &membership,
            }
            .execute("2@t", "s1", "")
            .unwrap();
            assert_eq!(
                (member.as_str(), perms),
                ("2@t", StorePermissions::member())
            );
            let denied = SignalStore {
                stores: &store_ports,
                access: &membership,
                log: &signal_log,
                clock: &SystemClock,
            }
            .execute("3@t", "s1", "x", &[], false)
            .unwrap_err();
            assert_eq!(
                legacy_error(denied).to_string(),
                "not allowed to signal in this store"
            );
        }

        /// LD-12: a deleted creature leaves every store, and stores left with no
        /// member are deleted.
        #[test]
        fn removing_a_member_everywhere_drops_memberships_and_empty_stores() {
            let trx = crate::core::trx::test_trx();
            crate::api::model::conformance::seed_humans(&trx, &["1@t", "2@t", "4@t"]);
            let stores = StorePorts { trx: &trx };
            let ports = MembershipPorts { trx: &trx };
            for id in ["s1", "s2"] {
                stores.create_store(&store(id), "4@t").unwrap();
            }
            let member = StorePermissions::member();
            ports.join("s1", "1@t", member).unwrap();
            ports.join("s1", "2@t", member).unwrap();
            ports.join("s2", "1@t", member).unwrap();
            // A membership needs its store: the relation is enforced.
            assert!(ports.join("gone", "1@t", member).is_err());

            let ids = |found: Vec<Store>| found.into_iter().map(|s| s.id).collect::<Vec<_>>();
            assert_eq!(ids(ports.member_stores("1@t", 50).unwrap()), ["s1", "s2"]);
            assert_eq!(ids(ports.member_stores("1@t", 1).unwrap()), ["s1"]);

            assert_eq!(ports.remove_member_everywhere("1@t").unwrap(), ["s2"]);
            assert!(ports.stores_of("1@t").unwrap().is_empty());
            assert!(ports.is_member("s1", "2@t").unwrap());
            assert!(stores.store("s1").unwrap().is_some());
            assert!(stores.store("s2").unwrap().is_none());
        }
    }
}
