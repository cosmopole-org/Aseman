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

use crate::core::actor::model::secured::guard::Guard;
use crate::models::action::ISecureAction;
use crate::models::core::ICore;
use crate::models::packet::{validate_tags, LogPacket, LogQuery};
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

/// Default page size for a history read when the caller names none.
const DEFAULT_HISTORY_COUNT: i64 = 100;

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
            let store_id = input.store_id.clone();
            if store_id.is_empty() {
                return Err(anyhow!("storeId is required"));
            }
            let sender_id = state.info().user_id();
            let trx = state.trx();
            let perms = read_permissions(&*trx, &store_id, &sender_id);
            if !perms.signal {
                return Err(anyhow!("not allowed to signal in this store"));
            }
            let tags = validate_tags(&input.tags)?;

            // `Store::pull` keeps the id it was handed whether or not the object
            // exists, so absence has to be read off the columns themselves — an
            // unknown store would otherwise look like a store that simply does
            // not keep history, and its messages would be dropped in silence.
            if trx.get_obj(Store::type_(), &store_id).is_empty() {
                return Err(anyhow!("store not found"));
            }
            let store = Store {
                id: store_id.clone(),
                ..Default::default()
            }
            .pull(&*trx);

            let mut sender = Creature {
                id: sender_id.clone(),
                ..Default::default()
            }
            .pull(&*trx);
            // Balance is never leaked over the signalling channel.
            sender.balance = 0;

            let now = chrono::Utc::now().timestamp_millis();
            let persist = store.pers_hist && !input.temp;
            // Recording comes FIRST, and its failure fails the whole call. A
            // signal fanned out to everyone's screen but missing from the log is
            // the worst outcome available: it looks delivered, and it is gone by
            // the next read. Better to tell the sender it did not send.
            let logged: Option<LogPacket> = if persist {
                let packet = app_for_handler.tools().storage().log_time_sieries(
                    &store_id,
                    &sender_id,
                    &input.data,
                    &tags,
                    now,
                )?;
                // Keep the store's own counter true to the log so a reader can
                // tell an empty store from an unreachable one.
                let mut counted = store.clone();
                counted.signal_count += 1;
                counted.push(&*trx);
                Some(packet)
            } else {
                None
            };

            let out = StoresSend {
                action: if input.typ.is_empty() {
                    "broadcast".to_string()
                } else {
                    input.typ.clone()
                },
                user: sender,
                store: Store {
                    id: store_id.clone(),
                    ..Default::default()
                },
                data: input.data.clone(),
                is_temp: input.temp,
                tags: tags.clone(),
                signal_id: logged.as_ref().map(|p| p.id.clone()).unwrap_or_default(),
                time: now,
                ..Default::default()
            };
            fan_out(app_for_handler.clone(), store_id, sender_id, out);

            Ok(json!({
                "passed": true,
                "persisted": persist,
                "signalId": logged.as_ref().map(|p| p.id.clone()).unwrap_or_default(),
                "time": now,
                "tags": tags,
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
            let store_id = input.store_id.clone();
            if store_id.is_empty() {
                return Err(anyhow!("storeId is required"));
            }
            let reader_id = state.info().user_id();
            let perms = read_permissions(&*state.trx(), &store_id, &reader_id);
            if !perms.read {
                return Err(anyhow!("not allowed to read this store"));
            }
            let query = LogQuery {
                tags_all: input.tags_all.clone(),
                tags_any: input.tags_any.clone(),
                before_time: input.before_time,
                after_time: input.after_time,
                count: if input.count > 0 {
                    input.count
                } else {
                    DEFAULT_HISTORY_COUNT
                },
            }
            .validated()?;
            let packets = app_for_handler
                .tools()
                .storage()
                .read_store_logs(&store_id, &query)?;
            Ok(json!({
                "storeId": store_id,
                "signals": packets,
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
            let store_id = input.store_id.clone();
            if store_id.is_empty() {
                return Err(anyhow!("storeId is required"));
            }
            if input.member_id.is_empty() {
                return Err(anyhow!("memberId is required"));
            }
            let caller = state.info().user_id();
            let trx = state.trx();
            if !read_permissions(&*trx, &store_id, &caller).manage {
                return Err(anyhow!("not allowed to manage access in this store"));
            }
            let perms = StorePermissions::from_list(&input.permissions);
            trx.put_link(
                &access_link_key(&store_id, &input.member_id),
                &perms.encode(),
            );
            Ok(json!({
                "storeId": store_id,
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
            let store_id = input.store_id.clone();
            if store_id.is_empty() {
                return Err(anyhow!("storeId is required"));
            }
            let caller = state.info().user_id();
            let target = if input.member_id.is_empty() {
                caller.clone()
            } else {
                input.member_id.clone()
            };
            let trx = state.trx();
            let caller_perms = read_permissions(&*trx, &store_id, &caller);
            if target != caller && !caller_perms.manage {
                return Err(anyhow!("not allowed to read another member's access"));
            }
            let perms = if target == caller {
                caller_perms
            } else {
                read_permissions(&*trx, &store_id, &target)
            };
            Ok(json!({
                "storeId": store_id,
                "memberId": target,
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
}
