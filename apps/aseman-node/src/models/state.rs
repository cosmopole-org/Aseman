//! Per-call state carried through a secured state modification.

use std::sync::Arc;

use crate::models::info::IInfo;
use crate::models::transaction::ITrx;

/// The mutable state handle threaded through every secured action.
///
/// It bundles the authenticated caller [`IInfo`], the live storage
/// [`ITrx`], and the request `source` string for audit/routing.
pub trait IState: Send + Sync {
    fn info(&self) -> Arc<dyn IInfo>;
    fn trx(&self) -> Arc<dyn ITrx>;
    fn set_trx(&self, trx: Arc<dyn ITrx>);
    fn source(&self) -> String;
    fn set_source(&self, source: &str);
}
