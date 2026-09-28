//! Per-call state carried through a secured state modification.

use std::sync::Arc;

use crate::models::info::IInfo;
use crate::core::trx::Trx;

/// The mutable state handle threaded through every secured action.
///
/// It bundles the authenticated caller [`IInfo`], the action's storage
/// [`Trx`], and the request `source` string for audit/routing.
pub trait IState: Send + Sync {
    fn info(&self) -> Arc<dyn IInfo>;
    fn trx(&self) -> Arc<Trx>;
    fn source(&self) -> String;
    fn set_source(&self, source: &str);
}
