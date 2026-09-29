//! Authentication of a signed packet on the signed-packet transports (TCP, WebSocket, the
//! chain, federation, and a guest's `execShellAction`).
//!
//! A packet names its creature and carries that creature's signature over its
//! payload. The operation's guard decides what is required:
//! - `public`: anonymous callers are admitted; a supplied signature must verify;
//! - `user`: a verified creature; a *machine* may instead present the in-process
//!   applet marker when the request comes from inside the node (a guest, or a
//!   packet the chain ordered);
//! - `store`: a `user` that is a member of the addressed store;
//! - `finance`: a verified signature, never the applet marker, so value only moves
//!   on the declared authority's own key.

use anyhow::{Result, anyhow};
use aseman_contracts::security::PacketGuard;
use serde_json::Value;

use super::{Caller, Operation};
use crate::node::Node;
use crate::state::creature_ports::CreaturePorts;

/// The signature a machine presents for itself from inside the node.
pub(crate) const APPLET_MARKER: &str = "#appletsign";

/// A signed packet: who signed, over what, with which signature.
pub(crate) struct SignedPacket<'a> {
    pub(crate) user_id: &'a str,
    pub(crate) payload: &'a [u8],
    pub(crate) signature: &'a str,
}

/// Where a packet reached the operation from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Entry {
    /// A client connection: only a real signature authenticates.
    Client,
    /// Inside the node (a guest's call) or ordered on the chain: a machine's
    /// applet marker also authenticates it.
    Inside,
}

fn verified(node: &Node, packet: &SignedPacket<'_>) -> bool {
    let (verified, _, _) = node.tools().security().auth_with_signature(
        packet.user_id,
        packet.payload,
        packet.signature,
    );
    verified
}

fn is_machine(node: &Node, user_id: &str) -> Result<bool> {
    node.in_action(|trx| {
        Ok(
            aseman_ports::CreatureDirectory::creature(&CreaturePorts { trx }, user_id)
                .map_err(|error| anyhow!("{error}"))?
                .is_some_and(|record| record.creature_type == "machine"),
        )
    })
}

/// The store a store-guarded packet addresses.
fn store_of(input: &Value) -> String {
    input
        .get("storeId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// Admit `packet` to `operation`: the caller it authenticates as, run from
/// `source`.
///
/// # Errors
///
/// `authorization failed` when the guard refuses.
pub(crate) fn admit(
    node: &Node,
    operation: &Operation,
    packet: &SignedPacket<'_>,
    input: &Value,
    entry: Entry,
    source: &str,
) -> Result<Caller> {
    let refused = || anyhow!("authorization failed");
    let in_store = operation.guard == PacketGuard::Store;
    let applet_allowed = matches!(operation.guard, PacketGuard::User | PacketGuard::Store);
    let user_id = packet.user_id;
    let mut store_id = String::new();
    if operation.guard == PacketGuard::Public {
        match (user_id.is_empty(), packet.signature.is_empty()) {
            (true, true) => {}
            (false, false) if verified(node, packet) => {}
            _ => return Err(refused()),
        }
    } else {
        let applet = applet_allowed
            && entry == Entry::Inside
            && packet.signature == APPLET_MARKER
            && is_machine(node, user_id)?;
        if !applet && !verified(node, packet) {
            return Err(refused());
        }
        if in_store {
            store_id = store_of(input);
            if !node
                .tools()
                .security()
                .has_access_to_store(user_id, &store_id)
            {
                return Err(refused());
            }
        }
    }
    Ok(Caller {
        user_id: user_id.to_owned(),
        store_id,
        source: source.to_owned(),
    })
}

/// Whether `packet` proves its creature's identity (a public operation also
/// admits an anonymous packet): the check before forwarding it to another node,
/// which authenticates it again.
pub(crate) fn identified(node: &Node, operation: &Operation, packet: &SignedPacket<'_>) -> bool {
    if operation.guard == PacketGuard::Public {
        match (packet.user_id.is_empty(), packet.signature.is_empty()) {
            (true, true) => return true,
            (false, false) => {}
            _ => return false,
        }
    }
    verified(node, packet)
}
