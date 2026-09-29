//! Bridge topics (`/gateway/*`): the subscription channel for programs that hold a socket
//! open but are not creatures.
//!
//! A creature's own VMs reach the node through their runtime's host calls.
//! Something running *beside* a VM — the crewAI bridge inside a Modal sandbox
//! — has neither a creature key nor a host ABI: it is an ordinary client
//! connection. It authenticates with a **bearer token its owning creature
//! minted** (`registerBridgeToken`), and that grant is the whole of its
//! authority: which topics it may subscribe to, and which creature its
//! signals are delivered to.
//!
//! ```text
//!   creature ── registerBridgeToken ─► grant (token hash → topics + owner)
//!   bridge   ── /gateway/subscribe ──► receives that topic's updates
//!   bridge   ── /gateway/signal ─────► signals the granting creature
//!   creature ── publishUpdate ───────► every subscriber of the topic
//! ```
//!
//! The token is never stored: only its SHA-256, so a state dump does not hand
//! anybody a working credential. Actions here take the public guard —
//! anonymous, because a bridge cannot sign; the token *is* the identity
//! check, and every one of these bodies performs it before doing anything.

use std::collections::HashMap;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use super::Ctx;
use super::wire::topic::{GatewaySignalInput, GatewaySubscribeInput, GatewayUnsubscribeInput};
use crate::storage::Trx;
use crate::util::future::async_once;

/// Hash a bearer token the way grants are keyed.
pub fn hash_bridge_token(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(token.trim().as_bytes());
    hex::encode(hasher.finalize())
}

/// A verified bridge grant.
pub struct BridgeGrant {
    /// The creature that minted the grant.
    pub creature_id: String,
    /// Where the bridge's inbound signals are delivered. Usually a *different*
    /// creature from the minter: a space's `create` action mints the grant,
    /// but the bridge's messages belong to the crew creature that handles
    /// them. Falls back to the minter when the grant names none.
    pub deliver_to: String,
    /// Per-action overrides of `deliver_to`, keyed by the exact action string.
    ///
    /// A bridge does more than one KIND of thing — it posts what its agents
    /// said, and it asks the platform to make a model call on its behalf — and
    /// those belong to different creatures. Without this a project's runtime
    /// could only ever reach one handler, so the LLM proxy would have to live
    /// inside the message creature purely because of how the token was minted.
    ///
    /// This is not an escalation: every entry is named by the creature that
    /// minted the grant, so the bridge still reaches exactly the handlers its
    /// owner nominated and nothing else.
    pub routes: HashMap<String, String>,
    pub topics: Vec<String>,
    pub expires_at: i64,
}

/// Build the creature signal envelope for a bridge call.
///
/// Bridge provenance deliberately lives on the outer node-created packet. A
/// caller controls `payload`, so putting `bridge` beside that payload inside
/// the user-shaped action object either loses it during normal signal
/// unwrapping or makes an untrusted payload indistinguishable from provenance.
pub(crate) fn bridge_signal_packet(
    input: &GatewaySignalInput,
    topic: &str,
    creature_id: &str,
) -> Value {
    let payload = json!({
        "action": input.action.trim(),
        "correlationId": input.correlation_id,
        "payload": input.payload,
    });
    json!({
        "action": "single",
        "entityId": "main",
        "bridge": {
            "topic": topic,
            "creatureId": creature_id,
        },
        "data": json!({
            "correlationId": input.correlation_id,
            "payload": payload.to_string(),
        })
        .to_string(),
    })
}

/// Read and validate the grant behind a bearer token.
///
/// Returns `None` for an unknown or expired token — the caller must not be
/// able to tell those apart, so both produce the same refusal.
pub fn resolve_bridge_grant(trx: &Trx, token: &str) -> Option<BridgeGrant> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    let hash = hash_bridge_token(token);
    let grant = crate::state::bridges::grant(trx, &hash).ok()??;
    let creature_id = grant["creatureId"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();
    if creature_id.is_empty() {
        return None;
    }
    let expires_at = grant["expiresAt"].as_i64().unwrap_or(0);
    if expires_at > 0 && chrono::Utc::now().timestamp_millis() > expires_at {
        return None;
    }
    let topics: Vec<String> = grant["topics"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let deliver_to = grant["deliverTo"]
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| creature_id.clone());
    let mut routes: HashMap<String, String> = HashMap::new();
    if let Some(entries) = grant["routes"].as_object() {
        for (action, target) in entries {
            let action = action.trim();
            let target = target.as_str().unwrap_or("").trim();
            if !action.is_empty() && !target.is_empty() {
                routes.insert(action.to_string(), target.to_string());
            }
        }
    }
    Some(BridgeGrant {
        creature_id,
        deliver_to,
        routes,
        topics,
        expires_at,
    })
}

fn grant(ctx: &Ctx<'_>, token: &str) -> Result<BridgeGrant> {
    resolve_bridge_grant(ctx.trx, token).ok_or_else(|| anyhow!("invalid or expired bridge token"))
}

/// Bind this connection to the topics a token grants (a request for specific
/// topics only narrows the grant). The transport, which owns the socket, attaches
/// it on the `gatewaySubscribe` it finds in the answer.
pub(super) fn subscribe(ctx: &Ctx<'_>, input: GatewaySubscribeInput) -> Result<Value> {
    let grant = grant(ctx, &input.token)?;
    let requested: Vec<String> = input
        .topics
        .iter()
        .map(|topic| topic.trim().to_owned())
        .filter(|topic| !topic.is_empty())
        .collect();
    let topics: Vec<String> = if requested.is_empty() {
        grant.topics.clone()
    } else {
        requested
            .into_iter()
            .filter(|topic| grant.topics.contains(topic))
            .collect()
    };
    if topics.is_empty() {
        return Err(anyhow!("token grants none of the requested topics"));
    }
    Ok(json!({
        "ok": true,
        "gatewaySubscribe": {
            "topics": topics,
            "creatureId": grant.creature_id,
        },
        "topics": topics,
        "creatureId": grant.creature_id,
        "expiresAt": grant.expires_at,
    }))
}

/// Stop receiving updates on this connection.
pub(super) fn unsubscribe(ctx: &Ctx<'_>, input: GatewayUnsubscribeInput) -> Result<Value> {
    grant(ctx, &input.token)?;
    Ok(json!({"ok": true, "gatewayUnsubscribe": true}))
}

/// A bridge asks its owning creature to do something. The creature is the one
/// the grant names for this action (else its default), never one the request
/// asks for, so a token reaches only the handlers its owner nominated. The
/// payload travels as a `creatures/signal` tagged with the bridge's topic.
pub(super) fn publish(ctx: &Ctx<'_>, input: GatewaySignalInput) -> Result<Value> {
    let grant = grant(ctx, &input.token)?;
    let topic = input.topic.trim().to_owned();
    if !topic.is_empty() && !grant.topics.contains(&topic) {
        return Err(anyhow!("token does not grant this topic"));
    }
    let action = input.action.trim().to_owned();
    if action.is_empty() {
        return Err(anyhow!("action is required"));
    }
    let packet = bridge_signal_packet(&input, &topic, &grant.creature_id);
    let creature_id = grant
        .routes
        .get(&action)
        .cloned()
        .unwrap_or_else(|| grant.deliver_to.clone());
    let signaler = ctx.node.tools().signaler();
    let target = creature_id.clone();
    async_once(move || signaler.signal_user("creatures/signal", &target, packet));
    Ok(json!({
        "ok": true,
        "creatureId": creature_id,
        "correlationId": input.correlation_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_provenance_is_outside_the_caller_payload() {
        let input = GatewaySignalInput {
            token: "secret".to_string(),
            topic: "space:alpha".to_string(),
            action: "crew/message".to_string(),
            correlation_id: "run-1".to_string(),
            payload: json!({"kind": "answer", "bridge": {"topic": "space:forged"}}),
        };

        let packet = bridge_signal_packet(&input, "space:alpha", "spaces-create");
        assert_eq!(packet["bridge"]["topic"], "space:alpha");
        assert_eq!(packet["bridge"]["creatureId"], "spaces-create");

        let data: Value = serde_json::from_str(packet["data"].as_str().unwrap()).unwrap();
        let inner: Value = serde_json::from_str(data["payload"].as_str().unwrap()).unwrap();
        assert!(inner.get("bridge").is_none());
        assert_eq!(inner["payload"]["bridge"]["topic"], "space:forged");
    }
}
