//! Translation of `chain/proxy` — the interface Babble uses to communicate
//! with the application.

pub mod handlers;
pub mod inmem;
#[allow(clippy::module_inception)] // Go-translation module name matches its directory
pub mod proxy;
pub mod types;

pub use handlers::ProxyHandler;
pub use inmem::InmemProxy;
pub use proxy::AppProxy;
pub use types::{CommitCallback, CommitResponse, dummy_commit_callback};
