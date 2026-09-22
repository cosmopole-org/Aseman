//! Helpers for building Rust action handlers without Go's
//! reflection-driven `ExtractAction` / `ExtractSecureAction`.
//!
//! Each action provides a `key`, a `Guard`, and a closure that runs against
//! an `Arc<dyn IState>` + a strongly-typed input. The helper wires those into
//! the `ISecureAction` registry the actor exposes; downcast from
//! `Arc<dyn IInput>` to the concrete `I` uses the `IInput::as_any` hook so
//! every field survives the trait round-trip.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::core::actor::model::base::action::{Action, ActionFn, StateModifierShared};
use crate::core::actor::model::secured::action::{Parse, SecureAction};
use crate::core::actor::model::secured::guard::Guard;
use crate::models::action::{IAction, ISecureAction};
use crate::models::core::ICore;
use crate::models::input::IInput;
use crate::models::state::IState;

/// Build a [`SecureAction`] from a typed closure + a [`Guard`].
///
/// Registered protocols (`tcp`, `ws`, `chain`, `fed`, `*`) all share a JSON
/// parser that deserialises into `I`. Inside the action body the closure
/// receives the concrete `I` value by downcasting through `IInput::as_any`,
/// so every field of the request is preserved exactly as sent.
pub fn build_secure_action<I, F>(
    app: Arc<dyn ICore>,
    key: &str,
    guard: Guard,
    func: F,
) -> Arc<dyn ISecureAction>
where
    I: IInput + DeserializeOwned + serde::Serialize + Default + Clone + Send + Sync + 'static,
    F: Fn(Arc<dyn IState>, I) -> Result<Value> + Send + Sync + 'static,
{
    let app_for_mod = app.clone();
    let modifier: StateModifierShared = Arc::new(move |readonly, closure| {
        app_for_mod.modify_state(readonly, closure);
    });
    let func = Arc::new(func);
    let func_for_action = func.clone();
    let path = key.to_owned();
    let action_fn: ActionFn = Arc::new(move |state: Arc<dyn IState>, input: Arc<dyn IInput>| {
        let typed: I = input
            .as_any()
            .downcast_ref::<I>()
            .cloned()
            .ok_or_else(|| anyhow!("action input type mismatch"))?;
        // Every signed action is authorized as its registered action (P4-05).
        let raw = serde_json::to_value(&typed).unwrap_or(Value::Null);
        let trx = state.trx();
        crate::shell::authority::authorize_shell_action(
            &crate::shell::authority::TrxLookups { trx: &*trx },
            &path,
            &state.info().user_id(),
            &raw,
            chrono::Utc::now().timestamp_millis(),
        )
        .map_err(|denied| anyhow!(denied))?;
        func_for_action(state, typed)
    });
    let inner_action: Arc<dyn IAction> = Arc::new(Action::new(modifier, key, action_fn));
    let mut parsers: std::collections::HashMap<String, Parse> = std::collections::HashMap::new();
    let parse: Parse = Arc::new(|raw: Value| {
        let parsed: I = serde_json::from_value(raw).unwrap_or_default();
        let boxed: Arc<dyn IInput> = Arc::new(parsed);
        Ok(boxed)
    });
    for proto in ["tcp", "ws", "chain", "fed", "*"] {
        parsers.insert(proto.to_string(), parse.clone());
    }
    Arc::new(SecureAction::new(inner_action, guard, app, parsers))
}
