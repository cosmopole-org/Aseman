//! A node for tests: every component, over the in-memory storage provider, with
//! nothing listening.

use std::collections::BTreeMap;
use std::sync::Arc;

use aseman_config::AsemanConfig;
use rsa::RsaPrivateKey;
use rsa::rand_core::OsRng;

use crate::identity::Security;
use crate::live::hub::Signaler;
use crate::node::{Node, Tools};
use crate::ratelimit::RateLimiter;
use crate::storage::NodeStorage;
use crate::transports::Network;
use crate::transports::chain::Blockchain;
use crate::transports::federation::FedNet;
use crate::workloads::NodeWorkloads;

impl Node {
    /// A loaded node `test-node`, owned by `1@global`, whose storage is a fresh
    /// in-memory provider, with its operations installed.
    pub(crate) fn for_tests() -> Arc<Node> {
        let root = std::env::temp_dir().join(format!(
            "aseman-node-test-{}",
            uuid::Uuid::now_v7().simple()
        ));
        let root = root.to_string_lossy().into_owned();
        let config = AsemanConfig::from_map(&BTreeMap::from([
            ("ASEMAN_NODE_ID".to_owned(), "1@global".to_owned()),
            ("ASEMAN_ORIGIN".to_owned(), "test-node".to_owned()),
            ("ASEMAN_STORAGE_ROOT_PATH".to_owned(), root.clone()),
            (
                "ASEMAN_CORE_STORAGE_PROVIDER".to_owned(),
                "rocksdb".to_owned(),
            ),
            (
                "ASEMAN_NODE_PRIVATE_KEY_SECRET".to_owned(),
                "secret://node/private-key".to_owned(),
            ),
        ]))
        .expect("a test configuration");
        let owner_key = RsaPrivateKey::new(&mut OsRng, 1024).expect("an owner key");
        let node = Node::new(Arc::new(config), owner_key);
        let storage = NodeStorage::new(&root, crate::storage::test_storage());
        let fed = FedNet::first_stage(node.clone());
        let signaler = Signaler::new(node.clone(), fed.clone());
        let security = Security::new(node.clone(), &root);
        let chain = Blockchain::new(node.clone(), &root);
        let network = Network::new(
            node.clone(),
            storage.clone(),
            security.clone(),
            signaler.clone(),
            fed.clone(),
            chain,
            None,
        );
        fed.second_stage(storage.clone(), signaler.clone());
        let tools = Arc::new(Tools {
            security,
            signaler,
            storage,
            network,
            workloads: NodeWorkloads::new(node.clone()),
            rate_limiter: RateLimiter::new(Default::default()),
        });
        assert!(node.tools.set(tools).is_ok(), "a fresh node");
        // The action plugins register into the SDK registry, as the runtime
        // phase does in `NodeApp::start` (ADR 0040).
        aseman_action_plugins::register_all();
        let router = crate::actions::Router::new(node.clone()).expect("the operation table");
        node.install_router(router);
        crate::actions::startup::install_creature_types(&node).expect("the creature types");
        node
    }
}
