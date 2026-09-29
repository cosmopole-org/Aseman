//! The signed-packet listeners the node runs: the chain, federation, TCP, and
//! WebSocket, each started on its configured port.

use std::collections::HashMap;
use std::sync::Arc;

use crate::identity::Security;
use crate::live::hub::Signaler;
use crate::node::Node;
use crate::storage::NodeStorage;
use crate::transports::chain::Blockchain;
use crate::transports::federation::FedNet;
use crate::transports::shell::tcp::Tcp;
use crate::transports::shell::ws::Ws;
use crate::transports::shell::{Tcp as TcpDriver, Ws as WsDriver};
use aseman_network_shell::TlsConfig;

/// The node's signed-packet listeners.
pub struct Network {
    _core: Arc<Node>,
    tcp: Arc<Tcp>,
    ws: Arc<Ws>,
    fed: Arc<FedNet>,
    chain: Arc<Blockchain>,
    tls_config: Option<TlsConfig>,
}

impl Network {
    /// `NewNetwork(core, storage, security, signaler, fed)`. The chain
    /// driver is constructed by the caller and passed in (as the
    /// it was wired via the chain package).
    pub fn new(
        core: Arc<Node>,
        _storage: Arc<NodeStorage>,
        _security: Arc<Security>,
        _signaler: Arc<Signaler>,
        fed: Arc<FedNet>,
        chain: Arc<Blockchain>,
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

impl Network {
    pub(crate) fn chain(&self) -> Arc<Blockchain> {
        self.chain.clone()
    }
    pub(crate) fn federation(&self) -> Arc<FedNet> {
        self.fed.clone()
    }
    pub(crate) fn run(&self, ports: HashMap<String, i64>) {
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
