//! The node's accessors and its transactions (ADR 0036).

use std::sync::Arc;

use anyhow::{Result, anyhow};

use crate::node::finance::VmCosts;
use crate::node::{Node, Tools};
use crate::state::core_storage::{StateFailure, run_action};
use crate::storage::Trx;
use crate::transports::chain::globe::Globe;
use crate::workloads::vmm::RemoteWorkloads;

impl Node {
    pub(crate) fn owner_id(&self) -> String {
        self.owner_id.clone()
    }

    pub(crate) fn id(&self) -> String {
        self.id.clone()
    }
    /// The node's components.
    ///
    /// # Panics
    ///
    /// Before [`Node::load`] ran: nothing reaches a component before the node
    /// is loaded.
    pub(crate) fn tools(&self) -> Arc<Tools> {
        self.tools
            .get()
            .cloned()
            .expect("the node's components are used only after it loads")
    }

    /// The chain globe.
    ///
    /// # Panics
    ///
    /// Before [`Node::load`] ran.
    pub(crate) fn globe(&self) -> Arc<Globe> {
        self.globe
            .get()
            .cloned()
            .expect("the chain globe is used only after the node loads")
    }

    /// The operations.
    ///
    /// # Panics
    ///
    /// Before the composition installed them ([`Node::install_router`]).
    pub(crate) fn router(&self) -> Arc<crate::actions::Router> {
        self.router
            .get()
            .cloned()
            .expect("the operations are installed before the node serves requests")
    }

    /// Install the operations the node serves (once).
    pub(crate) fn install_router(&self, router: Arc<crate::actions::Router>) {
        let _ = self.router.set(router);
    }

    /// Where guest data is served (`None` before the node's storage is open).
    pub(crate) fn guest_data(&self) -> Option<&crate::state::guest_data::GuestData> {
        self.guest_data.get()
    }

    /// The decision audit.
    pub(crate) fn audit(&self) -> &crate::state::audit::AuditLog {
        &self.audit
    }

    /// Bridge topic subscriptions.
    pub(crate) fn topics(&self) -> &crate::live::topics::Topics {
        &self.topics
    }

    /// The node's VMM, when one is configured.
    pub(crate) fn vmm(&self) -> Option<Arc<RemoteWorkloads>> {
        self.vmm.get().cloned()
    }

    /// What running on this node costs.
    pub(crate) fn vm_costs(&self) -> VmCosts {
        self.finance.costs()
    }

    /// The main port the node advertises (`/api/ping`).
    pub(crate) fn advertised_port(&self) -> String {
        self.config.services.main_port.clone()
    }

    pub(crate) fn close(&self) {
        if let Some(tools) = self.tools.get() {
            tools.network().chain().close();
        }
    }

    /// Run `action` in a read-write transaction: committed when it succeeds, rolled
    /// back when it refuses (LD-15).
    ///
    /// # Errors
    ///
    /// The action's refusal, or a transaction that cannot begin or commit.
    pub(crate) fn in_action<R>(&self, action: impl FnOnce(&Trx) -> Result<R>) -> Result<R> {
        self.transaction(false, action)
    }

    /// Run `read` in a read-only transaction.
    ///
    /// # Errors
    ///
    /// The read's error, or a transaction that cannot begin.
    pub(crate) fn read<R>(&self, read: impl FnOnce(&Trx) -> Result<R>) -> Result<R> {
        self.transaction(true, read)
    }

    fn transaction<R>(&self, readonly: bool, body: impl FnOnce(&Trx) -> Result<R>) -> Result<R> {
        self.run(readonly, body).map_err(StateFailure::into_error)
    }

    /// Run `body` in a transaction, telling a refusal of the body apart from a
    /// storage failure.
    pub(crate) fn run<R>(
        &self,
        readonly: bool,
        body: impl FnOnce(&Trx) -> Result<R>,
    ) -> Result<R, StateFailure> {
        let trx = self
            .tools()
            .storage()
            .begin(readonly)
            .map_err(StateFailure::Storage)?;
        let mut value = None;
        run_action(&trx, || {
            value = Some(body(&trx)?);
            Ok(())
        })?;
        value.ok_or_else(|| StateFailure::Storage(anyhow!("the transaction produced no value")))
    }

    /// Run `body` in a transaction that decides an `outcome` besides its writes (a
    /// refusal is an outcome, and the writes made before it commit); a storage
    /// failure is logged and leaves `initial`.
    pub(crate) fn with_outcome<T>(
        &self,
        readonly: bool,
        initial: T,
        body: impl FnOnce(&Trx, &mut T) -> Result<()>,
    ) -> T {
        let mut outcome = initial;
        if let Err(StateFailure::Storage(error)) = self.run(readonly, |trx| body(trx, &mut outcome))
        {
            eprintln!("storage: {error}");
        }
        outcome
    }

    /// Sign `data` with the node's own key (empty before its keys load).
    pub(crate) fn sign_packet(&self, data: &[u8]) -> String {
        self.node_key
            .get()
            .map_or_else(String::new, |key| Node::sign_with(key, data))
    }

    /// Sign `data` with the owner's key.
    pub(crate) fn sign_packet_as_owner(&self, data: &[u8]) -> String {
        Node::sign_with(&self.owner_key, data)
    }

    /// The consensus provider the chain installed, if any.
    pub(crate) fn consensus_provider(
        &self,
    ) -> Option<Arc<dyn aseman_ports::consensus::ConsensusProvider>> {
        self.tools.get()?.network().chain().consensus_provider()
    }
}
