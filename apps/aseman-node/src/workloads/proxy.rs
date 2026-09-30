//! Proxy entities — non-runnable creature program entities that forward
//! signals.
//!
//! A developer can deploy an entity with `entityType: "proxy"`. Such an
//! entity has no runnable module; it only holds a data file plus a target
//! descriptor (another program's entity, possibly of another creature). When
//! the proxy entity receives a signal, the node:
//!
//! 1. attaches the stored data file's content to the received payload
//!    (under the configured attach field),
//! 2. stamps a correlation id on the packet and records who sent it,
//! 3. forwards the repackaged signal to the target entity.
//!
//! When the target eventually signals back carrying the same correlation id
//! (addressed to the proxy's program), the node resolves the recorded
//! correlation and delivers the response to the original sender — with the
//! proxy entity's identity as the response sender, so the requester only
//! ever observes the proxy.
//!
//! **Streaming.** A correlation is not necessarily one message. A target may
//! send many responses on the same correlation id: every response marked as a
//! non-terminal *stream chunk* (`stream: true`, `final: false`, or a `kind`
//! ending in `/step`) is relayed to the original sender and the correlation is
//! *kept alive* (its expiry window refreshed) so more can follow; the record
//! is consumed only when a terminal message arrives. Responders that send a
//! single terminal message keep the original one-shot behavior unchanged.
//!
//! This is the substrate for "agent" deployments: an AI-agent skill file is
//! deployed as a proxy entity targeting the platform's agent backbone; every
//! request through the proxy reaches that backbone with the skill attached
//! (used as the session's system instruction), and the backbone streams its
//! trajectory (thoughts, tool steps) plus the final result back through the
//! proxy to the requester on one correlation.

use std::sync::Arc;

use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::actions::wire::store::Send as StoresSend;
use crate::node::Node;
use crate::state::Creature;
use crate::state::entity_ports::EntityPorts;
use crate::storage::{Trx, failed};
use aseman_domain::program::ArtifactRole;
use aseman_ports::{BlobStore, EntityDirectory};
use aseman_storage::client::core::proxy_correlation;
use aseman_storage::{FindMany, Models};

// The proxy deploy contract (the pseudo-runtime key and the config shape) is
// shared with the action plugins through `aseman-action-sdk` (ADR 0040); the
// routing half lives here.
pub use aseman_action_sdk::proxy::{DEFAULT_CORRELATION_TTL_MS, PROXY_RUNTIME_KEY, ProxyConfig};

fn value_as_ms(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The correlation record `correlation_id`, or an empty map.
fn correlation(trx: &Trx, correlation_id: &str) -> anyhow::Result<Map<String, Value>> {
    Ok(trx
        .proxy_correlation()
        .find_unique(proxy_correlation::by_key(correlation_id))
        .map_err(failed)?
        .and_then(|row| match row.record {
            Value::Object(record) => Some(record),
            _ => None,
        })
        .unwrap_or_default())
}

/// Record (or refresh) a correlation expiring at `expires_at`.
fn put_correlation(
    trx: &Trx,
    correlation_id: &str,
    record: &Value,
    expires_at: i64,
) -> anyhow::Result<()> {
    trx.proxy_correlation()
        .upsert(
            proxy_correlation::by_key(correlation_id),
            proxy_correlation::Create {
                key: correlation_id.to_owned(),
                expires_at_millis: expires_at,
                record: record.clone(),
            },
            proxy_correlation::update()
                .expires_at_millis(expires_at)
                .record(record.clone()),
        )
        .map(drop)
        .map_err(failed)
}

/// Delete a correlation record inside an open transaction.
fn delete_correlation(trx: &Trx, correlation_id: &str) -> anyhow::Result<()> {
    trx.proxy_correlation()
        .delete(proxy_correlation::by_key(correlation_id))
        .map(drop)
        .map_err(failed)
}

/// Whether a proxied response is a non-terminal *stream chunk*. The proxy keeps
/// the correlation alive for these and only consumes it on the terminal
/// message, so a target can stream many responses on one correlation.
///
/// A responder opts into streaming by marking a chunk with any of: `stream:
/// true`, `final: false`, or a `kind` ending in `/step` (or `/stream`). The
/// marker is read from the packet top-level or from its `data` JSON string (a
/// docker creature's raw result travels whole under `data`). Anything else is
/// terminal, preserving the original one-shot behavior for non-streaming
/// responders.
fn is_streaming_chunk(value: &Value) -> bool {
    fn from_obj(v: &Value) -> Option<bool> {
        if v.get("stream").and_then(Value::as_bool) == Some(true) {
            return Some(true);
        }
        if let Some(f) = v.get("final").and_then(Value::as_bool) {
            return Some(!f);
        }
        if let Some(kind) = v.get("kind").and_then(Value::as_str) {
            return Some(kind.ends_with("/step") || kind.ends_with("/stream"));
        }
        None
    }
    if let Some(b) = from_obj(value) {
        return b;
    }
    if let Some(data) = value.get("data").and_then(Value::as_str)
        && let Ok(parsed) = serde_json::from_str::<Value>(data)
        && let Some(b) = from_obj(&parsed)
    {
        return b;
    }
    false
}

/// Deep-merge `src` into `dst`: nested objects merge recursively, and any
/// non-object value in `src` overwrites `dst`. Used to overlay the proxy's
/// injected config onto a forwarded payload so the agent's stored fields (its
/// LLM key) win over anything the caller supplied.
fn deep_merge(dst: &mut Value, src: &Value) {
    match (dst, src) {
        (Value::Object(d), Value::Object(s)) => {
            for (k, v) in s {
                deep_merge(d.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
        (d, s) => {
            *d = s.clone();
        }
    }
}

/// `f` over a read-only transaction, or `default` when the transaction fails.
fn read_state<T>(app: &Arc<Node>, default: T, f: impl FnOnce(&Trx) -> T) -> T {
    app.read(|trx| Ok(f(trx))).unwrap_or(default)
}

/// The identity a proxied packet travels under: the proxy's program, with the
/// owning machine creature's username when available.
fn proxy_identity(app: &Arc<Node>, program_id: &str) -> Creature {
    let program_id_owned = program_id.to_string();
    read_state(app, Creature::default(), move |trx| {
        let program = (crate::state::program_ports::ProgramPorts { trx })
            .program_or_empty(&program_id_owned.clone());
        let owner = (crate::state::creature_ports::CreaturePorts { trx })
            .creature_or_empty(&program.machine_id.clone());
        Creature {
            id: program_id_owned.clone(),
            type_name: "machine".to_string(),
            username: if owner.username.is_empty() {
                program_id_owned.clone()
            } else {
                owner.username
            },
            ..Default::default()
        }
    })
}

/// Pull the correlation id out of an incoming `creatures/signal` payload:
/// either a top-level `correlationId` or one embedded in the packet's `data`
/// JSON string.
fn extract_correlation_id(value: &Value) -> String {
    fn from_obj(v: &Value) -> String {
        v.get("correlationId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }
    let c = from_obj(value);
    if !c.is_empty() {
        return c;
    }
    if let Some(data) = value.get("data").and_then(Value::as_str)
        && let Ok(parsed) = serde_json::from_str::<Value>(data)
    {
        let c = from_obj(&parsed);
        if !c.is_empty() {
            return c;
        }
        // Client convention: the caller's payload travels as a JSON
        // string under `data.payload` — the correlation id the requester
        // wants echoed back lives inside it.
        match parsed.get("payload") {
            Some(Value::String(p)) => {
                if let Ok(inner) = serde_json::from_str::<Value>(p) {
                    let c = from_obj(&inner);
                    if !c.is_empty() {
                        return c;
                    }
                }
            }
            Some(obj @ Value::Object(_)) => {
                let c = from_obj(obj);
                if !c.is_empty() {
                    return c;
                }
            }
            _ => {}
        }
    }
    String::new()
}

/// Response direction. If `value` carries a correlation id recorded by one of
/// `listener_machine_id`'s proxy entities, deliver it to the original sender
/// with the proxy's identity as the response sender, drop the correlation
/// record, and return `true`. Returns `false` when the packet is not a
/// proxied response for this machine.
pub fn try_route_proxy_response(app: &Arc<Node>, listener_machine_id: &str, value: &Value) -> bool {
    let correlation_id = extract_correlation_id(value);
    if correlation_id.is_empty() {
        return false;
    }
    let corr_owned = correlation_id.clone();
    let record = read_state(app, Map::new(), move |trx| {
        correlation(trx, &corr_owned).unwrap_or_default()
    });
    if record.is_empty() {
        return false;
    }
    let rec = |k: &str| {
        record
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    // Only the proxy that created the correlation may route the response —
    // the same correlation id travelling on the forwarded request must not
    // bounce at the target's own listener.
    if rec("proxyProgramId") != listener_machine_id {
        return false;
    }
    let sender_id = rec("senderId");
    if sender_id.is_empty() {
        return false;
    }
    // A correlation is only valid inside its lifetime window: a late
    // response consumes the stale record but is not delivered — the
    // requester has long since given up on it.
    let expires_at = record.get("expiresAt").and_then(value_as_ms).unwrap_or(0);
    if expires_at > 0 && now_ms() > expires_at {
        let corr_owned = correlation_id.clone();
        if let Err(error) = app.in_action(|trx: &Trx| delete_correlation(trx, &corr_owned)) {
            eprintln!("storage: {error}");
        }
        proxy_log(format!(
            "proxy correlation {} expired; dropping late response for {}",
            correlation_id, listener_machine_id
        ));
        return true;
    }
    let proxy_entity_id = rec("proxyEntityId");
    // Preserve the responder's payload; a non-Send-shaped packet (e.g. a raw
    // result object from a docker creature) is carried whole in `data`.
    let data = match value.get("data").and_then(Value::as_str) {
        Some(d) if !d.is_empty() => d.to_string(),
        _ => value.to_string(),
    };
    let response = StoresSend {
        user: proxy_identity(app, listener_machine_id),
        action: "single".to_string(),
        data,
        entity_id: proxy_entity_id,
        correlation_id: correlation_id.clone(),
        ..Default::default()
    };
    // A non-terminal stream chunk keeps the correlation alive (and refreshes its
    // expiry window so a long run cannot lapse mid-stream); only a terminal
    // message consumes the record. Non-streaming responders send a single
    // terminal message, so this preserves the original one-shot behavior.
    if is_streaming_chunk(value) {
        let ttl = record
            .get("ttlMs")
            .and_then(value_as_ms)
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_CORRELATION_TTL_MS);
        let new_expires_at = now_ms() + ttl;
        let mut refreshed = record.clone();
        refreshed.insert("expiresAt".to_string(), json!(new_expires_at));
        let corr_owned = correlation_id.clone();
        if let Err(error) = app.in_action(|trx: &Trx| {
            put_correlation(
                trx,
                &corr_owned,
                &Value::Object(refreshed.clone()),
                new_expires_at,
            )
        }) {
            eprintln!("storage: {error}");
        }
    } else {
        // Terminal: the round trip is complete — consume the record.
        let corr_owned = correlation_id.clone();
        if let Err(error) = app.in_action(|trx: &Trx| delete_correlation(trx, &corr_owned)) {
            eprintln!("storage: {error}");
        }
    }
    app.tools().signaler().signal_user(
        "creatures/signal",
        &sender_id,
        serde_json::to_value(&response).unwrap_or(Value::Null),
    );
    true
}

/// Request direction. If `entity_id` names a proxy entity of `machine_id`,
/// attach the entity's data file to the packet, record the correlation and
/// forward the repackaged signal to the configured target entity. Returns
/// `true` when the signal was consumed by a proxy entity.
pub fn try_forward_through_proxy(
    app: &Arc<Node>,
    machine_id: &str,
    entity_id: &str,
    value: &Value,
) -> bool {
    if entity_id.is_empty() {
        return false;
    }
    let machine_owned = machine_id.to_string();
    let entity_owned = entity_id.to_string();
    let (is_proxy, data_key, config_raw) = read_state(app, (false, None, Map::new()), move |trx| {
        let entities = EntityPorts { trx };
        let proxy = entities
            .entity(&machine_owned, &entity_owned)
            .ok()
            .flatten()
            .is_some_and(|entity| entity.entity_type == PROXY_RUNTIME_KEY);
        let primary = entities
            .artifact(&machine_owned, &entity_owned, ArtifactRole::Primary)
            .ok()
            .flatten();
        let Some(primary) = primary.filter(|_| proxy) else {
            return (false, None, Map::new());
        };
        let config = entities
            .entity_config(&machine_owned, &entity_owned)
            .ok()
            .flatten()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        (true, primary.store_key, config)
    });
    if !is_proxy {
        return false;
    }
    let config = ProxyConfig::from_value(&Value::Object(config_raw));
    if config.target_program_id.is_empty() {
        proxy_log(format!(
            "proxy entity {}::{} has no target configured; dropping signal",
            machine_id, entity_id
        ));
        return true;
    }
    let send: StoresSend = serde_json::from_value(value.clone()).unwrap_or_default();
    // Reuse the sender's correlation id when provided so the requester can
    // match the response; otherwise mint one for the round trip.
    let correlation_id = if !send.correlation_id.is_empty() {
        send.correlation_id.clone()
    } else {
        let embedded = extract_correlation_id(value);
        if embedded.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            embedded
        }
    };
    let attachment = data_key
        .and_then(|key| {
            crate::blobs::node_blobs(&app.tools().storage())
                .blob(&key)
                .ok()
                .flatten()
        })
        .map(|bytes| String::from_utf8(bytes).unwrap_or_default())
        .unwrap_or_default();
    // Attach the proxy's data file and routing envelope to the payload.
    let mut payload: Map<String, Value> = match serde_json::from_str::<Value>(&send.data) {
        Ok(Value::Object(o)) => o,
        _ => {
            let mut m = Map::new();
            if !send.data.is_empty() {
                m.insert("data".to_string(), json!(send.data));
            }
            m
        }
    };
    payload.insert(config.attach_field.clone(), json!(attachment));
    // Overlay the proxy's stored config (e.g. the agent's own `config.llm`) so
    // it reaches the backbone and wins over any caller-supplied value — a
    // client prompting a marketplace agent can neither read nor override the
    // agent's stored key.
    if config.inject.is_object() {
        let mut merged = Value::Object(payload);
        deep_merge(&mut merged, &config.inject);
        payload = match merged {
            Value::Object(o) => o,
            _ => Map::new(),
        };
    }
    payload.insert("correlationId".to_string(), json!(correlation_id));
    payload.insert("replyTo".to_string(), json!(machine_id));
    payload.insert("proxyProgramId".to_string(), json!(machine_id));
    payload.insert("proxyEntityId".to_string(), json!(entity_id));
    let created_at = now_ms();
    let expires_at = created_at + config.effective_correlation_ttl_ms();
    let record = json!({
        "senderId": send.user.id,
        "proxyProgramId": machine_id,
        "proxyEntityId": entity_id,
        "targetProgramId": config.target_program_id,
        "targetEntityId": config.target_entity_id,
        "createdAt": created_at,
        "expiresAt": expires_at,
        // Retained so a streamed chunk can refresh the expiry window by the
        // same lifetime the entity was configured with.
        "ttlMs": config.effective_correlation_ttl_ms(),
    });
    let corr_owned = correlation_id.clone();
    if let Err(error) =
        app.in_action(|trx: &Trx| put_correlation(trx, &corr_owned, &record, expires_at))
    {
        eprintln!("storage: {error}");
    }
    // Say where this goes. A proxy relay is otherwise completely invisible: the
    // requester sees only silence if the configured target no longer exists (a
    // backbone redeployed under a new program id leaves every proxy pointing at
    // a dead one), and nothing in any log ties the prompt to the id it was
    // actually sent to. One line per relay makes that a grep instead of a
    // deduction.
    proxy_log(format!(
        "proxy {}::{} -> {}::{} corr={}",
        machine_id, entity_id, config.target_program_id, config.target_entity_id, correlation_id
    ));
    let forwarded = StoresSend {
        user: proxy_identity(app, machine_id),
        action: "single".to_string(),
        // Carry the originating store through to the backbone untouched: the
        // requester scoped this signal to a space (store), and the agent behind
        // the proxy must see the same space the signal came from.
        store: send.store.clone(),
        data: Value::Object(payload).to_string(),
        is_temp: send.is_temp,
        entity_id: config.target_entity_id.clone(),
        correlation_id,
        ..Default::default()
    };
    app.tools().signaler().signal_user(
        "creatures/signal",
        &config.target_program_id,
        serde_json::to_value(&forwarded).unwrap_or(Value::Null),
    );
    true
}

/// Drop every correlation record whose lifetime has elapsed (found through the
/// model's expiry index).
pub fn sweep_expired_correlations(app: &Arc<Node>) {
    let now = now_ms();
    let expired = read_state(app, Vec::<String>::new(), move |trx| {
        trx.proxy_correlation()
            .find_many(FindMany::filter(
                proxy_correlation::expires_at_millis().lte(now),
            ))
            .map(|rows| rows.into_iter().map(|row| row.key).collect())
            .unwrap_or_default()
    });
    if expired.is_empty() {
        return;
    }
    let count = expired.len();
    if let Err(error) = app.in_action(|trx: &Trx| {
        for corr_id in &expired {
            delete_correlation(trx, corr_id)?;
        }
        Ok(())
    }) {
        eprintln!("storage: {error}");
    }
    proxy_log(format!(
        "proxy correlation reaper dropped {} expired record(s)",
        count
    ));
}

/// Spawn the background reaper that keeps the correlation store bounded even
/// when a target fails silently and never responds.
pub fn start_correlation_reaper(app: Arc<Node>) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
            sweep_expired_correlations(&app);
        }
    });
}

fn proxy_log(text: String) {
    eprintln!("[proxy] {text}");
}

#[cfg(test)]
mod inject_tests {
    // Exercises the REAL config parsing + deep-merge used by
    // try_forward_through_proxy, proving an agent's stored config.llm (with its
    // API key) is read from entity metadata, survives the stored-config
    // round-trip, and overrides a caller-supplied value while preserving the
    // caller's other fields.
    use super::{ProxyConfig, deep_merge};
    use aseman_action_sdk::proxy::config_from_metadata;
    use serde_json::{Map, Value, json};

    fn getter(meta: Value) -> impl Fn(&str) -> Option<Value> {
        let m: Map<String, Value> = match meta {
            Value::Object(o) => o,
            _ => Map::new(),
        };
        move |k: &str| m.get(k).cloned()
    }

    #[test]
    fn inject_parsed_roundtripped_and_overrides() {
        let get = getter(json!({
            "proxy": {
                "targetProgramId": "prog_backbone",
                "inject": { "config": { "llm": { "provider": "openai", "model": "gpt-5", "apiKey": "sk-SECRET" } } }
            }
        }));
        let cfg = config_from_metadata(get).expect("config");
        assert_eq!(
            cfg.inject.pointer("/config/llm/apiKey"),
            Some(&json!("sk-SECRET"))
        );

        // Stored-config round-trip (record_proxy_entity persists to_value()).
        let restored = ProxyConfig::from_value(&cfg.to_value());
        assert_eq!(
            restored.inject.pointer("/config/llm/model"),
            Some(&json!("gpt-5"))
        );

        // Merge onto a payload with caller tools + a forged llm.
        let mut payload = json!({
            "prompt": "hi",
            "config": { "tools": ["caspar__sandbox"], "llm": { "provider": "attacker", "apiKey": "sk-FORGED" } }
        });
        deep_merge(&mut payload, &restored.inject);
        assert_eq!(
            payload.pointer("/config/llm/apiKey"),
            Some(&json!("sk-SECRET")),
            "agent key wins"
        );
        assert_eq!(
            payload.pointer("/config/llm/provider"),
            Some(&json!("openai")),
            "agent provider wins"
        );
        assert_eq!(
            payload.pointer("/config/tools/0"),
            Some(&json!("caspar__sandbox")),
            "caller tools kept"
        );
        assert_eq!(
            payload.pointer("/prompt"),
            Some(&json!("hi")),
            "caller prompt kept"
        );
    }

    #[test]
    fn no_inject_leaves_payload_unchanged() {
        let get = getter(json!({ "proxy": { "targetProgramId": "p" } }));
        let cfg = config_from_metadata(get).expect("config");
        assert!(!cfg.inject.is_object(), "absent inject is not an object");
        let before = json!({ "prompt": "x", "config": { "llm": { "apiKey": "own" } } });
        let mut after = before.clone();
        if cfg.inject.is_object() {
            deep_merge(&mut after, &cfg.inject);
        }
        assert_eq!(after, before);
    }
}
