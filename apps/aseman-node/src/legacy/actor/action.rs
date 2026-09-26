//! Translation of `core/module/actor/model/base/action.go`.

use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;

use crate::models::action::{IAction, StateModifierFn, TrxClosure};
use crate::models::input::IInput;
use crate::models::state::IState;

/// Concrete callback signature the base [`Action`] dispatches into:
/// `func(state.IState, input.IInput) (any, error)`.
pub type ActionFn = Arc<dyn Fn(Arc<dyn IState>, Arc<dyn IInput>) -> Result<Value> + Send + Sync>;

/// Shared variant of [`StateModifierFn`]. Stored on [`Action`] so the
/// trait-returning `state_modifier` can hand out fresh `Box`es without
/// trying (and failing) to clone the inner closure.
pub type StateModifierShared = Arc<dyn Fn(bool, TrxClosure) + Send + Sync>;

/// Plain non-secured action: returns `(0, value)` on success and propagates
/// errors as `Err(...)` (matches Go's `(0, value, nil)` convention; non-zero
/// codes are reserved for error conditions).
pub struct Action {
    modifier: StateModifierShared,
    key: String,
    func: ActionFn,
}

impl Action {
    /// `NewAction(modifier, key, fn)`. Accepts the modifier as an [`Arc`] so
    /// multiple call sites can share it.
    pub fn new(modifier: StateModifierShared, key: &str, func: ActionFn) -> Action {
        Action {
            modifier,
            key: key.to_string(),
            func,
        }
    }
}

impl IAction for Action {
    fn state_modifier(&self) -> StateModifierFn {
        let m = self.modifier.clone();
        Box::new(move |readonly, closure| m(readonly, closure))
    }

    fn key(&self) -> String {
        self.key.clone()
    }

    fn act(&self, state: Arc<dyn IState>, input: Arc<dyn IInput>) -> Result<(i64, Value)> {
        let result = (self.func)(state, input)?;
        Ok((0, result))
    }
}
