//! Constructing a node.

use std::sync::{Arc, OnceLock};

use aseman_config::AsemanConfig;
use rsa::RsaPrivateKey;

use crate::node::Node;
use crate::node::finance::Finance;

impl Node {
    /// A node with `config`, owned by the creature `ASEMAN_NODE_ID` names, whose
    /// key is `owner_key`. Its id is its origin (`ASEMAN_ORIGIN`). Its
    /// components start with [`Node::load`].
    pub fn new(config: Arc<AsemanConfig>, owner_key: RsaPrivateKey) -> Arc<Node> {
        Arc::new(Node {
            id: config.node.origin.clone(),
            owner_id: config.node.id.clone(),
            owner_key: Arc::new(owner_key),
            finance: Finance::new(Some(&config)),
            config,
            tools: OnceLock::new(),
            globe: OnceLock::new(),
            node_key: OnceLock::new(),
            vmm: OnceLock::new(),
            router: OnceLock::new(),
            topics: Default::default(),
            audit: Default::default(),
            guest_data: OnceLock::new(),
        })
    }
}
