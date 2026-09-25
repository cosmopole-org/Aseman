//! Storage transaction abstractions.
//!
//! [`ITrx`] is the transaction handle threaded through every state-modifying
//! code path; [`IModel`] is the generic model contract layered on top of it.
//! [`helpers`] hosts the JSON map ↔ object conversion utilities used by
//! model implementations.

pub mod helpers;
pub mod transaction;

pub use helpers::{map_to_object, object_to_map};
pub use transaction::{IModel, ITrx};
