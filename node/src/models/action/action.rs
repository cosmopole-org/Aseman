use std::sync::Arc;

use anyhow::Result;
use serde_json::{Map, Value};

use crate::models::input::IInput;
use crate::models::state::IState;
use crate::models::transaction::ITrx;
use crate::util::AnyVal;

/// Closure handed to a state modifier — operates on an [`ITrx`].
pub type TrxClosure = Box<dyn FnMut(&dyn ITrx) -> Result<()> + Send>;

/// Function used to schedule a state modification. The `bool` is the
/// readonly flag; the closure runs against the opened transaction.
pub type StateModifierFn = Box<dyn Fn(bool, TrxClosure) + Send + Sync>;

/// A collection of actions that can be installed onto a state machine.
pub trait IActions: Send + Sync {
    fn install(&self, state: Arc<dyn IState>, args: Vec<AnyVal>);
}

/// A single action.
pub trait IAction: Send + Sync {
    fn state_modifier(&self) -> StateModifierFn;
    fn key(&self) -> String;
    fn act(&self, state: Arc<dyn IState>, input: Arc<dyn IInput>) -> Result<(i64, Value)>;
}

/// An action that performs its own authentication/authorization.
pub trait ISecureAction: Send + Sync {
    fn key(&self) -> String;
    /// Return the inner [`IAction`] so callers inside the same process can
    /// invoke the handler directly (without re-entering the secured path and
    /// deadlocking the state processor).
    fn inner_action(&self) -> Arc<dyn IAction>;
    fn has_global_parser(&self) -> bool;
    fn parse_input(&self, protocol: &str, raw: Value) -> Result<Arc<dyn IInput>>;
    #[allow(clippy::too_many_arguments)]
    fn securely_act(
        &self,
        user_id: &str,
        packet_id: &str,
        packet_binary: &[u8],
        packet_signature: &str,
        input: Arc<dyn IInput>,
        dummy: &str,
        insider: &[bool],
    ) -> Result<(i64, Value)>;
    #[allow(clippy::too_many_arguments)]
    fn securly_act_chain(
        &self,
        user_id: &str,
        packet_id: &str,
        packet_binary: &[u8],
        packet_signature: &str,
        input: Arc<dyn IInput>,
        origin: &str,
        tag: &str,
    ) -> Result<(i64, Value)>;
    fn securely_act_fed(
        &self,
        user_id: &str,
        packet_binary: &[u8],
        packet_signature: &str,
        input: Arc<dyn IInput>,
    ) -> Result<(i64, Value)>;
}

/// Describes a dynamically pluggable field on an entity (user, store, ...).
#[derive(Clone, Default)]
pub struct ExtendedField {
    pub name: String,
    pub path: String,
    pub typ: String,
    pub default: Value,
    pub required: bool,
    pub searchable: bool,
    pub primary_prop: bool,
    pub get_value:
        Option<Arc<dyn Fn(Arc<dyn IState>, Map<String, Value>) -> Result<Value> + Send + Sync>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extended_field_default_is_empty_and_inactive() {
        let f = ExtendedField::default();
        assert!(f.name.is_empty());
        assert!(f.path.is_empty());
        assert!(f.typ.is_empty());
        assert!(matches!(f.default, Value::Null));
        assert!(!f.required);
        assert!(!f.searchable);
        assert!(!f.primary_prop);
        assert!(f.get_value.is_none());
    }

    #[test]
    fn extended_field_clones_preserve_flags_and_callbacks() {
        let f = ExtendedField {
            name: "n".to_string(),
            path: "p".to_string(),
            typ: "string".to_string(),
            default: Value::String("d".to_string()),
            required: true,
            searchable: true,
            primary_prop: true,
            get_value: Some(Arc::new(|_state, _meta| Ok(Value::from(7)))),
        };
        let c = f.clone();
        assert_eq!(c.name, "n");
        assert_eq!(c.path, "p");
        assert_eq!(c.typ, "string");
        assert!(c.required && c.searchable && c.primary_prop);
        // The Arc<Fn ...> is cloned by reference, so both fields point at
        // the same callable.
        assert!(c.get_value.is_some());
    }
}
