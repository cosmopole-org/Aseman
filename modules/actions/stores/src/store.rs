//! The store handlers.

use anyhow::Result;
use aseman_action_sdk::state::creature_ports::CreaturePorts;
use aseman_action_sdk::state::store_ports::{
    MembershipPorts, SignalPorts, StorePorts, legacy_error, log_packet,
};
use aseman_action_sdk::state::Store;
use aseman_action_sdk::util::{Ctx, SystemClock, async_once};
use aseman_action_sdk::wire::store::{
    GetAccessInput, HistoryInput, Send as StorePacket, SetAccessInput, SignalInput,
};
use aseman_application::store::{GetStoreAccess, ReadStoreHistory, SetStoreAccess, SignalStore};
use aseman_domain::signal_tags::LogQuery;
use serde_json::{Value, json};

/// Send one signal into a store. Persistence is the store's decision; the sender
/// may only opt one signal out with `temp` (typing indicators, progress pings).
///
/// The live packet carries the persisted row's `signalId`, `time`, and `tags`, so
/// a member applies the same filter live as on history and recognises the
/// replayed row. Delivery resolves the store's members when it delivers, so a
/// member who joined during their session is reached.
pub fn signal(ctx: &Ctx<'_>, input: SignalInput) -> Result<Value> {
    let trx = ctx.trx;
    let sender_id = ctx.caller.user_id.clone();
    let outcome = SignalStore {
        stores: &StorePorts { trx },
        access: &MembershipPorts { trx },
        log: &SignalPorts { trx },
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
    let mut sender = CreaturePorts { trx }.creature_or_empty(&sender_id);
    // Balance never travels over the signalling channel.
    sender.balance = 0;
    let signal_id = outcome
        .signal
        .as_ref()
        .map(|signal| signal.id.clone())
        .unwrap_or_default();
    let packet = StorePacket {
        action: if input.typ.is_empty() {
            "broadcast".to_owned()
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
    let signaler = ctx.node.tools().signaler();
    let store_id = input.store_id.clone();
    let packet = serde_json::to_value(&packet)?;
    async_once(move || {
        signaler.signal_store("stores/signal", &store_id, packet, vec![sender_id], true);
    });
    Ok(json!({
        "passed": true,
        "persisted": outcome.persisted,
        "signalId": signal_id,
        "time": outcome.time_millis,
        "tags": outcome.tags,
    }))
}

/// Replay a store's persisted signals, newest first, filtered by tag.
pub fn history(ctx: &Ctx<'_>, input: HistoryInput) -> Result<Value> {
    let signals = ReadStoreHistory {
        access: &MembershipPorts { trx: ctx.trx },
        log: &SignalPorts { trx: ctx.trx },
    }
    .execute(
        &ctx.caller.user_id,
        &input.store_id,
        LogQuery {
            tags_all: input.tags_all,
            tags_any: input.tags_any,
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
}

/// Set one member's permissions (requires `manage`): a viewer holds `read`, a
/// member `read,signal`, an administrator `read,signal,manage`.
pub fn set_access(ctx: &Ctx<'_>, input: SetAccessInput) -> Result<Value> {
    let permissions = SetStoreAccess {
        access: &MembershipPorts { trx: ctx.trx },
    }
    .execute(
        &ctx.caller.user_id,
        &input.store_id,
        &input.member_id,
        &input.permissions,
    )
    .map_err(legacy_error)?;
    Ok(json!({
        "storeId": input.store_id,
        "memberId": input.member_id,
        "permissions": permissions,
    }))
}

/// A member's permissions: a member may read their own; reading another's
/// requires `manage`.
pub fn get_access(ctx: &Ctx<'_>, input: GetAccessInput) -> Result<Value> {
    let (member, permissions) = GetStoreAccess {
        access: &MembershipPorts { trx: ctx.trx },
    }
    .execute(&ctx.caller.user_id, &input.store_id, &input.member_id)
    .map_err(legacy_error)?;
    Ok(json!({
        "storeId": input.store_id,
        "memberId": member,
        "permissions": permissions,
    }))
}