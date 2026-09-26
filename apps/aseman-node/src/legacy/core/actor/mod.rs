//! Action dispatch system.
//!
//! - `registry` — the concrete [`Actor`] holding the action tables.
//! - `model/base` — identity context (`Info`) and the simple [`Action`].
//! - `model/secured` — secured action wrapper + auth guard.
//! - `model/state` — the state carrier passed to modifier closures.
//! - `model/trx` — the RocksDB-backed [`model::trx::TrxWrapper`] layer.

pub mod model;
pub mod registry;

pub use model::base::action::Action;
pub use model::base::info::Info;
pub use model::secured::action::SecureAction;
pub use model::secured::guard::Guard;
pub use model::state::State;
pub use registry::Actor;
