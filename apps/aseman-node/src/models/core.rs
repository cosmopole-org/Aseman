//! [`ICore`] — the central node orchestrator that owns the action
//! registry, the driver bundle, the globe view of the chain, and the
//! state-modification entry points.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::action::TrxClosure;
use crate::models::action::actor::IActor;
use crate::models::chain::Effects;
use crate::models::globe::IGlobe;
use crate::models::info::IInfo;
use crate::models::ports::tools::ITools;
use crate::models::state::IState;
use crate::models::transaction::ITrx;

/// An empty action payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmptyPayload {}

/// Holds a response payload together with its produced effects.
#[derive(Debug, Clone, Default)]
pub struct ResponseHolder {
    pub payload: Vec<u8>,
    pub effects: Effects,
}

/// Closure handed to a secured state modification: `func(state.IState) error`.
pub type StateClosure = Box<dyn FnMut(Arc<dyn IState>) -> Result<()> + Send>;

/// The node core — central registry and orchestrator.
pub trait ICore: Send + Sync {
    fn owner_id(&self) -> String;
    fn id(&self) -> String;
    fn gods(&self) -> Vec<String>;
    fn add_god(&self, username: &str);
    fn tools(&self) -> Arc<dyn ITools>;
    fn free_nodes(&self) -> HashMap<String, bool>;
    fn add_free_node(&self, node_id: &str);
    fn actor(&self) -> Arc<dyn IActor>;
    fn load(&self, args: Vec<String>, config: HashMap<String, Value>);
    fn close(&self);
    fn plant_chain_trigger(
        &self,
        count: i64,
        user_id: &str,
        tag: &str,
        machine_id: &str,
        store_id: &str,
        input: &str,
    );
    fn app_pending_trxs(&self);
    fn ip_addr(&self) -> String;
    fn modify_state(&self, readonly: bool, fn_: TrxClosure);
    fn modify_state_securly_with_source(
        &self,
        readonly: bool,
        info: Arc<dyn IInfo>,
        src: &str,
        fn_: StateClosure,
    );
    fn modify_state_securly(&self, readonly: bool, info: Arc<dyn IInfo>, fn_: StateClosure);
    /// [`Self::modify_state_securly_with_source`] that reports its outcome: the
    /// closure's error (its writes are discarded) or a failed commit (LD-10/LD-15).
    fn modify_state_securly_checked(
        &self,
        readonly: bool,
        info: Arc<dyn IInfo>,
        src: &str,
        fn_: StateClosure,
    ) -> Result<()> {
        self.modify_state_securly_with_source(readonly, info, src, fn_);
        Ok(())
    }
    fn sign_packet(&self, data: &[u8]) -> String;
    fn sign_packet_as_owner(&self, data: &[u8]) -> String;
    fn execution_cost_per_second(&self) -> i64;
    fn vm_ram_cost_per_mb_per_minute(&self) -> i64;
    fn vm_cpu_core_cost_per_minute(&self) -> i64;
    fn vm_disk_cost_per_gb_per_minute(&self) -> i64;
    fn globe(&self) -> Arc<dyn IGlobe>;
}
