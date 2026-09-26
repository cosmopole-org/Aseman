//! The core actor compatibility subsystem — the `Actor` registry and its action model.
//!
//! - [`actor`] — the concrete [`Actor`] registry holding action tables.
//! - [`action`] — the plain non-secured [`Action`] and its helper types.
//! - [`info`] — the identity context ([`Info`]) attached to state calls.
//! - [`secure_action`] — the secured [`SecureAction`] wrapper + input parsers.
//! - [`guard`] — the auth [`Guard`] for secured actions.
//! - [`state`] — the [`State`] carrier threaded through secured modifications.
//!
//! This is the translation of `core/module/actor` (RL-003); its entries
//! migrate to `aseman-application` use cases as the strangler proceeds.

pub mod action;
pub mod actor;
pub mod guard;
pub mod info;
pub mod secure_action;
pub mod state;

pub use actor::Actor;
pub use guard::Guard;
pub use info::Info;
pub use state::State;
