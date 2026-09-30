//! The node's implementation of the action SDK's host interfaces (ADR 0040):
//! the [`ActionNode`] facade over the node's components, and the [`ActionContext`]
//! one operation runs with.

use std::collections::BTreeMap;
use std::sync::Arc;

use aseman_action_sdk::context::{
    ActionNetwork, ActionSecurity, ActionSignaler, ActionStorage, ActionTools, ActionVmm,
    ActionWorkloads, NodeFacade,
};
use aseman_action_sdk::util::LaunchResources;
use aseman_action_sdk::{ActionCaller, ActionContext, VmCosts};
use aseman_domain::identity::Subject;
use aseman_domain::vmm::{DeployConventions, LogRecord};
use aseman_domain::{DesiredWorkloadState, WorkloadId};
use aseman_storage::Trx;
use serde_json::Value;

use crate::identity::Security;
use crate::live::hub::Signaler;
use crate::node::Node;
use crate::storage::NodeStorage;
use crate::transports::Network;
use crate::workloads::NodeWorkloads;
use crate::workloads::vmm::RemoteWorkloads;

/// The node as an action plugin sees it.
pub(crate) struct NodeActionNode {
    node: Arc<Node>,
}

impl NodeActionNode {
    pub(crate) fn new(node: Arc<Node>) -> Self {
        Self { node }
    }
}

impl aseman_action_sdk::ActionNode for NodeActionNode {
    fn id(&self) -> String {
        self.node.id()
    }

    fn owner_id(&self) -> String {
        self.node.owner_id()
    }

    fn advertised_port(&self) -> String {
        self.node.advertised_port()
    }

    fn tools(&self) -> ActionTools {
        let tools = self.node.tools();
        ActionTools::new(
            Arc::new(NodeActionStorage(tools.storage())),
            Arc::new(NodeActionSecurity(tools.security())),
            Arc::new(NodeActionSignaler(tools.signaler())),
            Arc::new(NodeActionWorkloads(tools.workloads())),
            Arc::new(NodeActionNetwork(tools.network())),
        )
    }

    fn vmm(&self) -> Option<Arc<dyn ActionVmm>> {
        self.node.vmm().map(|remote| {
            let remote: Arc<dyn ActionVmm> = Arc::new(NodeActionVmm {
                node: self.node.clone(),
                remote,
            });
            remote
        })
    }

    fn vm_costs(&self) -> VmCosts {
        self.node.vm_costs()
    }

    fn sign_packet_as_owner(&self, data: &[u8]) -> String {
        self.node.sign_packet_as_owner(data)
    }
}

/// One operation's context: the node facade, the operation's transaction, and
/// the caller.
pub(crate) struct NodeActionContext<'a> {
    facade: &'a NodeFacade,
    trx: &'a Trx,
    caller: &'a ActionCaller,
}

impl<'a> NodeActionContext<'a> {
    pub(crate) fn new(facade: &'a NodeFacade, trx: &'a Trx, caller: &'a ActionCaller) -> Self {
        Self {
            facade,
            trx,
            caller,
        }
    }
}

impl ActionContext for NodeActionContext<'_> {
    fn node(&self) -> &NodeFacade {
        self.facade
    }

    fn trx(&self) -> &Trx {
        self.trx
    }

    fn caller(&self) -> &ActionCaller {
        self.caller
    }
}

// ── Service adapters ─────────────────────────────────────────────────────────

/// Storage: id minting, the master key, and the blob store.
pub(crate) struct NodeActionStorage(pub(crate) Arc<NodeStorage>);

impl ActionStorage for NodeActionStorage {
    fn gen_id(&self, origin: &str) -> String {
        self.0.gen_id(origin)
    }

    fn master_key(&self) -> anyhow::Result<[u8; 32]> {
        self.0.master_key()
    }

    fn storage_root(&self) -> String {
        self.0.storage_root()
    }

    fn blob_store(&self) -> Arc<aseman_action_sdk::blobs::StorageRootBlobStore> {
        Arc::new(aseman_action_sdk::blobs::StorageRootBlobStore::new(
            self.0.storage_root(),
        ))
    }
}

/// Security: signature verification and store access.
pub(crate) struct NodeActionSecurity(pub(crate) Arc<Security>);

impl ActionSecurity for NodeActionSecurity {
    fn auth_with_signature(
        &self,
        user_id: &str,
        payload: &[u8],
        signature: &str,
    ) -> (bool, String, bool) {
        self.0.auth_with_signature(user_id, payload, signature)
    }

    fn has_access_to_store(&self, user_id: &str, store_id: &str) -> bool {
        self.0.has_access_to_store(user_id, store_id)
    }

    fn fetch_key_pair(&self, tag: &str) -> Vec<Vec<u8>> {
        self.0.fetch_key_pair(tag)
    }
}

/// The signaler: live fan-out to users, groups, and stores.
pub(crate) struct NodeActionSignaler(pub(crate) Arc<Signaler>);

impl ActionSignaler for NodeActionSignaler {
    fn signal_user(&self, key: &str, user_id: &str, data: Value) {
        self.0.signal_user(key, user_id, data);
    }

    fn signal_group(&self, key: &str, group_id: &str, data: Value, exceptions: Vec<String>) {
        self.0.signal_group(key, group_id, data, exceptions);
    }

    fn signal_store(
        &self,
        key: &str,
        store_id: &str,
        data: Value,
        exceptions: Vec<String>,
        federate: bool,
    ) {
        self.0.signal_store(key, store_id, data, exceptions, federate);
    }

    fn join_group(&self, group_id: &str, user_id: &str) {
        self.0.join_group(group_id, user_id);
    }
}

/// Workload signal delivery.
pub(crate) struct NodeActionWorkloads(pub(crate) Arc<NodeWorkloads>);

impl ActionWorkloads for NodeActionWorkloads {
    fn assign(&self, program_id: &str) {
        self.0.assign(program_id);
    }

    fn run_vm_entity(&self, machine_id: &str, store_id: &str, data: &str, entity_id: &str) {
        self.0.run_vm_entity(machine_id, store_id, data, entity_id);
    }
}

/// The network: peers, for the node diagnostics.
pub(crate) struct NodeActionNetwork(pub(crate) Arc<Network>);

impl ActionNetwork for NodeActionNetwork {
    fn peers(&self) -> Vec<String> {
        self.0.chain().peers()
    }
}

/// The VMM, when the node has one.
pub(crate) struct NodeActionVmm {
    node: Arc<Node>,
    remote: Arc<RemoteWorkloads>,
}

impl ActionVmm for NodeActionVmm {
    fn workload_id(&self, program: &str, entity: &str, vm: &str) -> WorkloadId {
        RemoteWorkloads::workload_id(program, entity, vm)
    }

    fn launch(
        &self,
        program: &str,
        machine: &str,
        entity: &str,
        vm: &str,
        runtime: &str,
        resources: LaunchResources,
        environment: BTreeMap<String, String>,
    ) -> anyhow::Result<WorkloadId> {
        self.remote
            .launch(program, machine, entity, vm, runtime, resources, environment)
    }

    fn set_state(
        &self,
        user: &str,
        workload: WorkloadId,
        state: DesiredWorkloadState,
    ) -> anyhow::Result<u64> {
        self.remote.set_state(user, workload, state)
    }

    fn set_state_as(
        &self,
        actor: Subject,
        workload: WorkloadId,
        state: DesiredWorkloadState,
    ) -> anyhow::Result<u64> {
        self.remote.set_state_as(actor, workload, state)
    }

    fn offers(&self, runtime: &str) -> bool {
        self.remote.offers(runtime)
    }

    fn runtime_keys(&self) -> Vec<String> {
        self.remote.runtime_keys()
    }

    fn logs(&self, workload: WorkloadId, after: u64) -> anyhow::Result<Vec<LogRecord>> {
        self.remote.logs(workload, after)
    }

    fn deploy_conventions(&self, runtime: &str) -> Option<DeployConventions> {
        self.remote.deploy_conventions(runtime)
    }

    fn vm_host_call(&self, op: &str, caller: &str, input: &Value) -> String {
        self.remote.vm_host_call(&self.node, op, caller, input)
    }
}