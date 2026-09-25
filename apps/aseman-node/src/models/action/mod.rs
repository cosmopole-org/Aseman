//! The action layer.
//!
//! - [`IAction`] / [`IActions`] — pluggable units of work installed onto
//!   a state machine.
//! - [`ISecureAction`] — actions that perform their own
//!   authentication/authorization.
//! - [`IActor`] — the per-node registry that holds both flavours.
//! - [`IPlugger`] — entry point a plugin uses to expose its actions.

pub mod action;
pub mod actor;
pub mod plugger;

pub use action::{ExtendedField, IAction, IActions, ISecureAction, StateModifierFn, TrxClosure};
pub use actor::IActor;
pub use plugger::IPlugger;
