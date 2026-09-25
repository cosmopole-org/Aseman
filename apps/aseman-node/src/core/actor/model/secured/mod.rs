//! Translation of `core/module/actor/model/secured`.

pub mod action;
pub mod guard;

pub use action::{Parse, SecureAction};
pub use guard::Guard;
