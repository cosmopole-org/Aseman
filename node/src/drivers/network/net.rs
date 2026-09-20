//! Translation of `drivers/network/net.go`.
//!
//! `Network` aggregates the four sub-drivers — `chain`, `federation`,
//! `tcp`, `ws` — into a single [`INetwork`] facade. `Run(ports)` boots each
//! sub-driver on the port the caller specified.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;

use crate::drivers::network::client::{Tcp as TcpDriver, Ws as WsDriver};
use crate::models::core::ICore;
use crate::models::ports::network::chain::IChain;
use crate::models::ports::network::federation::IFederation;
use crate::models::ports::network::tcp::ITcp;
use crate::models::ports::network::ws::IWs;
use crate::models::ports::network::{INetwork, TlsConfig};
use crate::models::ports::security::ISecurity;
use crate::models::ports::signaler::ISignaler;
use crate::models::ports::storage::IStorage;

/// Concrete [`INetwork`] implementation.
pub struct Network {
    _core: Arc<dyn ICore>,
    tcp: Arc<dyn ITcp>,
    ws: Arc<dyn IWs>,
    fed: Arc<dyn IFederation>,
    chain: Arc<dyn IChain>,
    tls_config: Option<TlsConfig>,
}

impl Network {
    /// `NewNetwork(core, storage, security, signaler, fed)`. The chain
    /// driver is constructed by the caller and passed in (matches Go where
    /// it was wired via the chain package).
    pub fn new(
        core: Arc<dyn ICore>,
        _storage: Arc<dyn IStorage>,
        _security: Arc<dyn ISecurity>,
        _signaler: Arc<dyn ISignaler>,
        fed: Arc<dyn IFederation>,
        chain: Arc<dyn IChain>,
        tls_config: Option<TlsConfig>,
    ) -> Arc<Network> {
        let tcp = TcpDriver::new(core.clone());
        let ws = WsDriver::new(core.clone());
        Arc::new(Network {
            _core: core,
            tcp,
            ws,
            fed,
            chain,
            tls_config,
        })
    }
}

impl INetwork for Network {
    fn chain(&self) -> Arc<dyn IChain> {
        self.chain.clone()
    }
    fn federation(&self) -> Arc<dyn IFederation> {
        self.fed.clone()
    }
    fn tcp(&self) -> Arc<dyn ITcp> {
        self.tcp.clone()
    }
    fn ws(&self) -> Arc<dyn IWs> {
        self.ws.clone()
    }
    fn tls_config(&self) -> Option<TlsConfig> {
        self.tls_config.clone()
    }
    fn run(&self, ports: HashMap<String, i64>) {
        if let Some(p) = ports.get("tcp") {
            self.tcp.listen(*p, self.tls_config.clone());
        }
        if let Some(p) = ports.get("ws") {
            self.ws.listen(*p, self.tls_config.clone());
        }
        if let Some(p) = ports.get("fed") {
            self.fed.listen(*p, self.tls_config.clone());
        }
        if let Some(p) = ports.get("chain") {
            self.chain.listen(*p, self.tls_config.clone());
        }
    }
}

#[allow(dead_code)]
fn _force_use() -> Result<()> {
    Ok(())
}
