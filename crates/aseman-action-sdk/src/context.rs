//! The host-side API surface an action plugin's handler may call.
//!
//! The node implements [`ActionNode`] once (backed by its canonical
//! `Node → tools()` object graph) and publishes it through a [`NodeFacade`]
//! when the router is built. Each operation runs with an [`ActionContext`] —
//! the facade, the operation's transaction, and the caller. Plugins never see
//! node internals: everything they need flows through these narrow interfaces,
//! which lets the compiler verify every interaction between an action project
//! and the node.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Result;
use aseman_domain::identity::Subject;
use aseman_domain::vmm::{DeployConventions, LogRecord};
use aseman_domain::{DesiredWorkloadState, WorkloadId};
use aseman_storage::Trx;
use serde_json::Value;

use crate::caller::ActionCaller;

/// The legacy root identity (`/creatures/mint`'s hard-coded administrator).
pub const LEGACY_ROOT: &str = "1@global";

/// A handler's context: the node facade, the operation's transaction, and the
/// caller.
pub trait ActionContext: Send + Sync {
    /// The node's published services.
    fn node(&self) -> &NodeFacade;
    /// The operation's transaction; it commits when the handler succeeds.
    fn trx(&self) -> &Trx;
    /// Who the operation runs for.
    fn caller(&self) -> &ActionCaller;
}

/// The published node services a handler may call.
#[derive(Clone)]
pub struct NodeFacade {
    inner: Arc<dyn ActionNode>,
}

impl NodeFacade {
    /// Publish `inner` as the node's action surface.
    #[must_use]
    pub fn new(inner: Arc<dyn ActionNode>) -> Self {
        Self { inner }
    }

    /// The node's id, which is also its origin.
    #[must_use]
    pub fn id(&self) -> String {
        self.inner.id()
    }

    /// The creature that owns the node.
    #[must_use]
    pub fn owner_id(&self) -> String {
        self.inner.owner_id()
    }

    /// The main port the node advertises (`/api/ping`).
    #[must_use]
    pub fn advertised_port(&self) -> String {
        self.inner.advertised_port()
    }

    /// The node's components (storage, security, the signaler, workloads, the
    /// network).
    #[must_use]
    pub fn tools(&self) -> ActionTools {
        self.inner.tools()
    }

    /// The node's VMM, when one is configured.
    #[must_use]
    pub fn vmm(&self) -> Option<Arc<dyn ActionVmm>> {
        self.inner.vmm()
    }

    /// What running on this node costs.
    #[must_use]
    pub fn vm_costs(&self) -> VmCosts {
        self.inner.vm_costs()
    }

    /// Sign `data` with the node owner's key.
    #[must_use]
    pub fn sign_packet_as_owner(&self, data: &[u8]) -> String {
        self.inner.sign_packet_as_owner(data)
    }
}

/// The node's components as an action plugin sees them.
#[derive(Clone)]
pub struct ActionTools {
    storage: Arc<dyn ActionStorage>,
    security: Arc<dyn ActionSecurity>,
    signaler: Arc<dyn ActionSignaler>,
    workloads: Arc<dyn ActionWorkloads>,
    network: Arc<dyn ActionNetwork>,
}

impl ActionTools {
    #[must_use]
    pub fn new(
        storage: Arc<dyn ActionStorage>,
        security: Arc<dyn ActionSecurity>,
        signaler: Arc<dyn ActionSignaler>,
        workloads: Arc<dyn ActionWorkloads>,
        network: Arc<dyn ActionNetwork>,
    ) -> Self {
        Self {
            storage,
            security,
            signaler,
            workloads,
            network,
        }
    }

    /// Id minting, the master key, and the blob store.
    #[must_use]
    pub fn storage(&self) -> Arc<dyn ActionStorage> {
        self.storage.clone()
    }

    /// Signature verification and store access.
    #[must_use]
    pub fn security(&self) -> Arc<dyn ActionSecurity> {
        self.security.clone()
    }

    /// Live signal fan-out.
    #[must_use]
    pub fn signaler(&self) -> Arc<dyn ActionSignaler> {
        self.signaler.clone()
    }

    /// Workload signal delivery.
    #[must_use]
    pub fn workloads(&self) -> Arc<dyn ActionWorkloads> {
        self.workloads.clone()
    }

    /// Network peers (for diagnostics).
    #[must_use]
    pub fn network(&self) -> Arc<dyn ActionNetwork> {
        self.network.clone()
    }
}

/// The node as an action plugin sees it.
pub trait ActionNode: Send + Sync {
    /// The node's id, which is also its origin.
    fn id(&self) -> String;
    /// The creature that owns the node.
    fn owner_id(&self) -> String;
    /// The main port the node advertises (`/api/ping`).
    fn advertised_port(&self) -> String;
    /// The node's components.
    fn tools(&self) -> ActionTools;
    /// The node's VMM, when one is configured.
    fn vmm(&self) -> Option<Arc<dyn ActionVmm>>;
    /// What running on this node costs.
    fn vm_costs(&self) -> VmCosts;
    /// Sign `data` with the node owner's key.
    fn sign_packet_as_owner(&self, data: &[u8]) -> String;
}

/// Storage: id minting, the master key, and the blob store.
pub trait ActionStorage: Send + Sync {
    /// A fresh legacy id minted in `origin`'s id space.
    fn gen_id(&self, origin: &str) -> String;
    /// The node master key (loaded or created on first use).
    fn master_key(&self) -> Result<[u8; 32]>;
    /// The node's storage root.
    fn storage_root(&self) -> String;
    /// The blob store under the storage root.
    fn blob_store(&self) -> Arc<crate::blobs::StorageRootBlobStore>;
}

/// Security: signature verification and store access.
pub trait ActionSecurity: Send + Sync {
    /// Verify a creature's signature over `payload`; `(verified, type, _)`.
    fn auth_with_signature(
        &self,
        user_id: &str,
        payload: &[u8],
        signature: &str,
    ) -> (bool, String, bool);
    /// Whether `user_id` may access `store_id`.
    fn has_access_to_store(&self, user_id: &str, store_id: &str) -> bool;
    /// The stored key pairs for `tag` (`fetch_key_pair("server_key")` returns
    /// the node's own keys).
    fn fetch_key_pair(&self, tag: &str) -> Vec<Vec<u8>>;
}

/// The signaler: live fan-out to users, groups, and stores.
pub trait ActionSignaler: Send + Sync {
    fn signal_user(&self, key: &str, user_id: &str, data: Value);
    fn signal_group(&self, key: &str, group_id: &str, data: Value, exceptions: Vec<String>);
    fn signal_store(
        &self,
        key: &str,
        store_id: &str,
        data: Value,
        exceptions: Vec<String>,
        federate: bool,
    );
    fn join_group(&self, group_id: &str, user_id: &str);
}

/// Workload signal delivery (the program side of the VMM).
pub trait ActionWorkloads: Send + Sync {
    /// Register the program's signal listener.
    fn assign(&self, program_id: &str);
    /// Run a program's entity for a store signal.
    fn run_vm_entity(&self, machine_id: &str, store_id: &str, data: &str, entity_id: &str);
}

/// The network: peers, for the node diagnostics.
pub trait ActionNetwork: Send + Sync {
    /// The chain peers.
    fn peers(&self) -> Vec<String>;
}

/// The VMM, when the node has one (ADR 0029).
pub trait ActionVmm: Send + Sync {
    /// The workload id of one VM instance of a program entity.
    fn workload_id(&self, program: &str, entity: &str, vm: &str) -> WorkloadId;
    /// Record, key, and create one VM instance of `entity`.
    #[allow(clippy::too_many_arguments)] // the VM launch surface
    fn launch(
        &self,
        program: &str,
        machine: &str,
        entity: &str,
        vm: &str,
        runtime: &str,
        resources: crate::util::LaunchResources,
        environment: BTreeMap<String, String>,
    ) -> Result<WorkloadId>;
    /// Change a workload's desired state as `user`, the owner of its program.
    fn set_state(&self, user: &str, workload: WorkloadId, state: DesiredWorkloadState)
        -> Result<u64>;
    /// Change a workload's desired state as `actor`, whose ownership the node's
    /// decision point already established.
    fn set_state_as(
        &self,
        actor: Subject,
        workload: WorkloadId,
        state: DesiredWorkloadState,
    ) -> Result<u64>;
    /// Whether the VMM offers `runtime`.
    fn offers(&self, runtime: &str) -> bool;
    /// The runtime keys the VMM offers.
    fn runtime_keys(&self) -> Vec<String>;
    /// A workload's log lines after `after`, oldest first.
    fn logs(&self, workload: WorkloadId, after: u64) -> Result<Vec<LogRecord>>;
    /// The deploy conventions of `runtime`.
    fn deploy_conventions(&self, runtime: &str) -> Option<DeployConventions>;
    /// A VM host call (`statusVm`, …) for `caller`; the answer keeps the wire
    /// shapes.
    fn vm_host_call(&self, op: &str, caller: &str, input: &Value) -> String;
}

/// What running on this node costs.
#[derive(Clone, Copy, Debug, Default)]
pub struct VmCosts {
    pub execution_per_second: i64,
    pub ram_per_mb_minute: i64,
    pub cpu_core_per_minute: i64,
    pub disk_per_gb_minute: i64,
}

impl VmCosts {
    /// Whether VMs are free here (every VM rate is zero).
    #[must_use]
    pub fn is_free(&self) -> bool {
        self.ram_per_mb_minute == 0 && self.cpu_core_per_minute == 0 && self.disk_per_gb_minute == 0
    }
}