//! Network port traits — the four transports plus the top-level
//! [`INetwork`] facade that bundles them.
//!
//! - `chain` — consensus chain transport (`IChain`)
//! - `federation` — federation transport (`IFederation`)
//! - `tcp`, `ws` — client transports (`ITcp`, `IWs`)
//!
//! The top-level [`INetwork`] trait lives here. Compatibility TLS material is
//! owned by `aseman-network-legacy` with its wire implementation.

use std::collections::HashMap;
use std::sync::Arc;

use aseman_network_legacy::TlsConfig;

pub mod chain;
pub mod federation;
pub mod tcp;
pub mod ws;

pub use chain::IChain;
pub use federation::IFederation;
pub use tcp::ITcp;
pub use ws::IWs;

/// The top-level network driver interface.
pub trait INetwork: Send + Sync {
    fn chain(&self) -> Arc<dyn IChain>;
    fn federation(&self) -> Arc<dyn IFederation>;
    fn tcp(&self) -> Arc<dyn ITcp>;
    fn ws(&self) -> Arc<dyn IWs>;
    fn tls_config(&self) -> Option<TlsConfig>;
    fn run(&self, ports: HashMap<String, i64>);
}
