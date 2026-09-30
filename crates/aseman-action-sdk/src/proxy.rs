//! Proxy entities — non-runnable creature program entities that forward
//! signals.
//!
//! The workload plugin deploys a proxy entity with `entityType: "proxy"`. The
//! deploy half of the proxy contract is pure — a stored data file plus a target
//! descriptor — so it lives here, shared with the plugin. The routing half (the
//! correlation reaper, forwarding, response routing) is node-side and stays in
//! `apps/aseman-node/src/workloads/proxy.rs`.

use aseman_domain::blob::BlobEvidence;
use aseman_domain::program::EntityRecord;
use serde_json::{Value, json};

use crate::state::entity_ports::EntityPorts;
use crate::util::Trx;

/// The pseudo-runtime key a proxy entity is deployed under. It is not a VM
/// runtime: nothing ever runs for a proxy entity.
pub const PROXY_RUNTIME_KEY: &str = "proxy";

/// Default payload field the proxy's data file content is attached under.
pub const DEFAULT_ATTACH_FIELD: &str = "attachment";

/// Default lifetime of a correlation record (ms). A target that never
/// responds must not leak its correlation record forever: after this window
/// the record is consumed by the reaper (or by a late response, which is
/// then dropped). Sized to comfortably outlast a long streaming run (e.g. a
/// an agent's wall-clock budget); each streamed chunk also refreshes the
/// window, so an actively-streaming correlation never expires mid-run.
pub const DEFAULT_CORRELATION_TTL_MS: i64 = 20 * 60 * 1000;

/// Floor for a per-entity configured TTL, so a typo cannot make records
/// expire before the target has any chance to answer.
const MIN_CORRELATION_TTL_MS: i64 = 1_000;

/// Normalized proxy-entity configuration.
#[derive(Debug, Clone, Default)]
pub struct ProxyConfig {
    pub target_program_id: String,
    pub target_entity_id: String,
    pub attach_field: String,
    /// Correlation-record lifetime in ms; `0` means the node default.
    pub correlation_ttl_ms: i64,
    /// Static fields the proxy deep-merges into every forwarded payload,
    /// stored in the entity's (private) config — never in public metadata.
    pub inject: Value,
}

impl ProxyConfig {
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut v = json!({
            "targetProgramId": self.target_program_id,
            "targetEntityId": self.target_entity_id,
            "attachField": self.attach_field,
            "correlationTtlMs": self.correlation_ttl_ms,
        });
        if self.inject.is_object() {
            v.as_object_mut()
                .unwrap()
                .insert("inject".to_string(), self.inject.clone());
        }
        v
    }

    #[must_use]
    pub fn from_value(v: &Value) -> ProxyConfig {
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let mut cfg = ProxyConfig {
            target_program_id: s("targetProgramId"),
            target_entity_id: s("targetEntityId"),
            attach_field: s("attachField"),
            correlation_ttl_ms: v.get("correlationTtlMs").and_then(value_as_ms).unwrap_or(0),
            inject: v
                .get("inject")
                .filter(|x| x.is_object())
                .cloned()
                .unwrap_or(Value::Null),
        };
        if cfg.attach_field.is_empty() {
            cfg.attach_field = DEFAULT_ATTACH_FIELD.to_string();
        }
        cfg
    }

    /// The effective correlation-record lifetime for this proxy entity.
    #[must_use]
    pub fn effective_correlation_ttl_ms(&self) -> i64 {
        if self.correlation_ttl_ms <= 0 {
            DEFAULT_CORRELATION_TTL_MS
        } else {
            self.correlation_ttl_ms.max(MIN_CORRELATION_TTL_MS)
        }
    }
}

fn value_as_ms(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// Extract the proxy configuration from a deploy request's metadata. Accepts
/// either a nested `"proxy": {...}` object or flat `proxyTarget*` keys.
pub fn config_from_metadata<M>(get: M) -> Result<ProxyConfig, String>
where
    M: Fn(&str) -> Option<Value>,
{
    let nested = get("proxy").unwrap_or(Value::Null);
    let pick = |nested_key: &str, flat_key: &str| -> String {
        nested
            .get(nested_key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                get(flat_key)
                    .and_then(|v| v.as_str().map(str::to_string))
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_default()
    };
    let mut target_program_id = pick("targetProgramId", "proxyTargetProgramId");
    if target_program_id.is_empty() {
        target_program_id = pick("targetMachineId", "proxyTargetMachineId");
    }
    if target_program_id.is_empty() {
        target_program_id = pick("targetCreatureId", "proxyTargetCreatureId");
    }
    if target_program_id.is_empty() {
        return Err(
            "proxy entity deploy requires metadata.proxy.targetProgramId (the program/creature the proxy forwards to)"
                .to_string(),
        );
    }
    let target_entity_id = pick("targetEntityId", "proxyTargetEntityId");
    let mut attach_field = pick("attachField", "proxyAttachField");
    if attach_field.is_empty() {
        attach_field = DEFAULT_ATTACH_FIELD.to_string();
    }
    let correlation_ttl_ms = nested
        .get("correlationTtlMs")
        .and_then(value_as_ms)
        .or_else(|| get("proxyCorrelationTtlMs").as_ref().and_then(value_as_ms))
        .unwrap_or(0);
    // Static payload injection (e.g. the agent's own `config.llm`). Accept it
    // nested under `proxy.inject` or as a flat `proxyInject` key; keep only an
    // object so a stray scalar cannot corrupt the forwarded payload.
    let inject = nested
        .get("inject")
        .cloned()
        .or_else(|| get("proxyInject"))
        .filter(|x| x.is_object())
        .unwrap_or(Value::Null);
    Ok(ProxyConfig {
        target_program_id,
        target_entity_id,
        attach_field,
        correlation_ttl_ms,
        inject,
    })
}

/// Record a deployed proxy entity inside an open state transaction: the entity
/// itself, its stored data file as the primary file, and the proxy target
/// configuration.
pub fn record_proxy_entity(
    trx: &Trx,
    program_id: &str,
    entity_id: &str,
    data: &BlobEvidence,
    config: &ProxyConfig,
) -> anyhow::Result<()> {
    aseman_application::program::RecordEntityDeployment {
        entities: &EntityPorts { trx },
    }
    .execute(&aseman_application::program::EntityDeployment {
        entity: EntityRecord {
            program_id: program_id.to_string(),
            entity_id: entity_id.to_string(),
            entity_type: PROXY_RUNTIME_KEY.to_string(),
            image_name: entity_id.to_string(),
        },
        primary: data.clone(),
        runtime_file: true,
        downloadable: false,
        config: Some(config.to_value().to_string()),
    })
    .map_err(|error| anyhow::anyhow!("{error}"))
}